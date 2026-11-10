#!/usr/bin/env python3
"""Generate services.tsv: the in-process service catalogue for the measured
hello-views launch + interaction binder work-lists, with call counts injected
from research/dynamic-trace/data/binder_calls_*.tsv.

Rows are hand-designed (reply values, statefulness, risk); counts are measured.
Rows with launch=interaction=0 are required by the framework but were not
observed in the trace; the `notes` column says why (parser gap / pre-profile).
"""
import csv, os

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(HERE, "..", "dynamic-trace", "data")

# (interface, method) -> (launch, interaction) actual counts
counts = {}
for phase, f in (("launch", "binder_calls_launch.tsv"),
                 ("interaction", "binder_calls_interaction.tsv")):
    for r in csv.DictReader(open(os.path.join(DATA, f)), delimiter="\t"):
        if r["direction"] != "app->system":
            continue
        k = (r["interface"], r["method"])
        c = counts.setdefault(k, {"launch": 0, "interaction": 0})
        c[phase] += int(r["count"])

# Columns:
# interface, owner, method, signature, state, reply, consumer, risk, notes
OUT = [
# ---------------- IServiceManager: the chokepoint ----------------
("android.os.IServiceManager", "svc.registry", "checkService",
 "(Ljava/lang/String;)Landroid/os/IBinder;", "STATEFUL",
 "Binder handle for the registered service (a Rust-owned local Binder whose descriptor equals the requested name). Unknown name -> null, and the Java side then throws/degrades exactly as on Android.",
 "ServiceManager.getService/checkService -> every other interface on this list.",
 "M", "9 calls at launch = 9 service lookups. This is the only place ACOS decides which 'system services' exist; unknown names must return null, never a silent stub."),
("android.os.IServiceManager", "svc.registry", "getService",
 "(Ljava/lang/String;)Landroid/os/IBinder;", "STATEFUL",
 "Same table as checkService.", "ServiceManager.getService.",
 "M", "Not hit by hello-views (checkService is used) but every framework helper tries it."),
("android.os.IServiceManager", "svc.registry", "addService",
 "(Ljava/lang/String;Landroid/os/IBinder;ZI)V", "STATEFUL",
 "Ignored unless ACOS is the caller.", "ServiceManager.addService (system_server only).",
 "L", "Not reachable from an app process; implement as an error, not a success."),

# ---------------- IDisplayManager ----------------
("android.hardware.display.IDisplayManager", "svc.display", "getPreferredWideGamutColorSpaceId",
 "()I", "STATELESS", "1 = ColorSpace.Named.SRGB (emulator panel is sRGB).",
 "Display.getPreferredWideGamutColorSpace / DisplayManagerGlobal; only used when an app asks for wide gamut.",
 "L", "INFERRED value. Falsify by logging the id the framework derives and comparing to the reference screenshot's colour handling."),
("android.hardware.display.IDisplayManager", "svc.display", "getOverlaySupport",
 "()Landroid/hardware/OverlayProperties;", "STATELESS",
 "OverlayProperties with supportFp16ForHdr=false, supportMixedColorSpaces=false. NOTE: read back by *our* native (nReadOverlayPropertiesFromParcel), so ACOS owns both ends of this format.",
 "DisplayManagerGlobal -> Display.getHdrCapabilities.",
 "L", "No HDR on the software renderer; document as a capability limit."),
("android.hardware.display.IDisplayManager", "svc.display", "getDisplayInfo",
 "(I)Landroid/view/DisplayInfo;", "STATELESS",
 "displayId=0, name='Built-in Screen', type=TYPE_INTERNAL(1), state=ON(2), logicalWidth=1080, logicalHeight=2400, appWidth=1080, appHeight=2209, rotation=ROTATION_0(0), logicalDensityDpi=420, refreshRate=60.000004, supportedModes=[1080x2400@60.000004 modeId=1], colorMode=COLOR_MODE_SRGB, installOrientation=0. Parcel layout derived mechanically from DisplayInfo.writeToParcel (codegen), not hand-written.",
 "DisplayManagerGlobal.getDisplayInfoLocked -> Display -> Resources metrics/Configuration; called 25x at launch.",
 "M", "Values above are measured from fixtures/reference/hello-views/art-api34-x86_64/display.txt (init=1080x2400 420dpi app=1080x2209)."),
("android.hardware.display.IDisplayManager", "svc.display", "getDisplayIds",
 "(Z)[I", "STATELESS", "{0} (a single default display).",
 "DisplayManagerGlobal.getDisplayIds -> DisplayManager.getDisplayIds.",
 "L", "A second (virtual) display id must be added only when a task asks for it."),
("android.hardware.display.IDisplayManager", "svc.display", "registerCallbackWithEventMask",
 "(Landroid/hardware/display/IDisplayManagerCallback;J)V", "STATEFUL",
 "Record the callback binder; emit onDisplayEvent only if ACOS changes display state (rotation/resize). Never emit unsolicited events.",
 "DisplayManagerGlobal.registerCallbackIfNeededLocked -> DisplayManager.DisplayListener.",
 "L", "Callback must be delivered through the main looper queue, not synchronously."),

# ---------------- IConnectivityManager ----------------
("android.net.IConnectivityManager", "svc.net", "getProxyForNetwork",
 "(Landroid/net/Network;)Landroid/net/ProxyInfo;", "STATELESS",
 "null (no proxy configured).", "Proxy.getProxy -> networking defaults.", "L",
 "1 call at launch. Returning null is what a proxyless device does."),

# ---------------- INetworkManagementService ----------------
("android.os.INetworkManagementService", "svc.net", "setUidCleartextNetworkPolicy",
 "(II)V", "STATELESS", "Accept and record (uid, policy); oneway in practice.",
 "NetworkSecurityPolicy.getInstance() -> cleartext policy reported to the app.",
 "L", "Policy is advisory here; there is no network in ACOS V1."),

# ---------------- IPackageManager (mini-PMS) ----------------
("android.content.pm.IPackageManager", "svc.pms", "notifyDexLoad",
 "(Ljava/lang/String;Ljava/util/Map;Ljava/lang/String;)V", "STATELESS",
 "Accept and drop (oneway).", "ActivityThread/DexLoadReporter during class-loader setup.",
 "L", "Used for dexopt bookkeeping only."),
("android.content.pm.IPackageManager", "svc.pms", "getPackagesForUid",
 "(I)[Ljava/lang/String;", "STATELESS",
 "['dev.acos.fixtures.helloviews'] for uid 10192.",
 "Process/SharedPreferences/ContextImpl package attribution.",
 "L", "Single-package process in V1."),
("android.content.pm.IPackageManager", "svc.pms", "getApplicationInfo",
 "(Ljava/lang/String;JI)Landroid/content/pm/ApplicationInfo;", "STATELESS",
 "ApplicationInfo from the APK manifest (crates/acos-apk): packageName=dev.acos.fixtures.helloviews, sourceDir/publicSourceDir=<acos vfs path of base.apk>, dataDir=/data/user/0/<pkg>, nativeLibraryDir=<acos vfs lib/x86_64>, uid=10192 (measured: display.txt 'A=10192'), targetSdkVersion=34, minSdkVersion=26, theme=0, className=<app class>, processName=<pkg>, enabled=true, enabledSetting=0, targetSandboxVersion=2, flags=0, category=-1. Materialise the Java object and let ApplicationInfo.writeToParcel emit the parcel.",
 "ActivityThread.getPackageInfoNoCheck -> LoadedApk (class loader, data dir, native lib dir) for bindApplication and every Activity start.",
 "H", "Largest reply on the launch path (~60 fields in API 34). Must be stateful-free but exact; every field the framework reads is observable."),
("android.content.pm.IPackageManager", "svc.pms", "getPackageInfo",
 "(Ljava/lang/String;JI)Landroid/content/pm/PackageInfo;", "STATELESS",
 "PackageInfo: versionCode=1, versionName=<manifest>, firstInstallTime/lastUpdateTime=0, packageName, activities=[MainActivity with name/exported/theme/label/launchMode/configChanges], applicationInfo (as above), requestedPermissions from the manifest, signatures=[synthetic], minSdkVersion=26, targetSdkVersion=34.",
 "ActivityThread / ContextImpl.getPackageManager().getPackageInfo (version checks, activity meta-data).",
 "H", "Only the fields the fixture reads are needed, but the parcel must be complete: derive the write order from PackageInfo.writeToParcel."),
("android.content.pm.IPackageManager", "svc.pms", "hasSystemFeature",
 "(Ljava/lang/String;I)Z", "STATELESS",
 "Explicit feature table: android.hardware.touchscreen=true, android.hardware.faketouch=true, android.software.app_widgets=false, android.software.webview=false, everything else=false. Log the first 100 queried names at INFO to close the gap.",
 "PackageManager.hasSystemFeature -> gates widgets/animations/IME behaviour.",
 "M", "The 3 launch queries' *names* are not in the trace (native args untraced). See gap G-2."),
("android.content.pm.IPackageManager", "svc.pms", "getInstrumentationInfoAsUser",
 "(Ljava/lang/String;II)Landroid/content/pm/InstrumentationInfo;", "STATELESS",
 "null (no instrumentation).", "ActivityThread at attach (returns null on Android too).",
 "L", "Required for completeness of the attach path."),
("android.content.pm.IPackageManager", "svc.pms", "checkPermission",
 "(Ljava/lang/String;II)I", "STATELESS",
 "PERMISSION_GRANTED(0) only for permissions the fixture declares and the user granted (-g install); otherwise PERMISSION_DENIED(-1). Manifest-driven, not hard-coded.",
 "ContextImpl.checkPermission / Settings read enforcement.",
 "M", "Not in the measured 51 (only IActivityManager.checkPermission is) but the same policy object serves both."),

# ---------------- IActivityManager (mini-ATMS) ----------------
("android.app.IActivityManager", "svc.atms", "checkPermission",
 "(Ljava/lang/String;II)I", "STATELESS",
 "Manifest/user-grant driven (see PMS.checkPermission); 4 calls at launch.",
 "ContextImpl/Settings/DeviceConfig permission checks during app init.",
 "M", "Measured 4x. Returning GRANTED for everything would silently change observable behaviour (e.g. Settings read restrictions)."),
("android.app.IActivityManager", "svc.atms", "finishAttachApplication",
 "(J)V", "STATEFUL",
 "Ack the attach sequence: record startSeq, mark the process attached, and (real Android) trigger the first bindApplication. ACOS: after this returns, deliver bindApplication on the main looper.",
 "ActivityThread.handleBindApplication caller chain (attach handshake).",
 "M", "Part of the in-process 'process start' handshake; see REPORT section 6."),
("android.app.IActivityManager", "svc.atms", "getContentProvider",
 "(Landroid/app/IApplicationThread;Ljava/lang/String;Ljava/lang/String;IZ)Landroid/app/ContentProviderHolder;", "STATEFUL",
 "For 'settings': ContentProviderHolder{provider=<our IContentProvider stub>, info=ProviderInfo{authority='settings', packageName='com.android.providers.settings', readPermission='android.permission.READ_...'}, connection=null, delay=false}. Unknown authority -> null.",
 "ActivityThread.acquireProvider -> Settings reads (Settings$NameValueCache), DeviceConfig.",
 "H", "The provider is then *called* (IContentProvider.call x5) - see that row. This is the only ContentProvider on the V1 path."),
("android.app.IActivityManager", "svc.atms", "setRenderThread",
 "(I)V", "STATEFUL",
 "Record the RenderThread tid for the process record (no behaviour).",
 "ActivityThread/ThreadedRenderer after the render thread starts.",
 "L", "Bookkeeping only; ACOS's render thread is Rust."),
("android.app.IActivityManager", "svc.atms", "attachApplication",
 "(Landroid/app/IApplicationThread;J)V", "STATEFUL",
 "Entry into the mini-ATMS: record the app thread binder, then schedule bindApplication (see finishAttachApplication).",
 "ActivityThread.attach -> ActivityManager.getService().attachApplication.",
 "H", "NOT in the measured 51: it fires before the profiler starts (REPORT: handleBindApplication already at 27.7 ms). ACOS must synthesise this direction itself."),
("android.app.IActivityManager", "svc.atms", "registerStrictModeCallback",
 "(Landroid/os/IBinder;)V", "STATELESS", "Accept; never invoke.", "StrictMode init.",
 "L", "Static work-list only; harmless if absent."),
("android.app.IActivityManager", "svc.atms", "showWaitingForDebugger",
 "(Landroid/app/IApplicationThread;Z)V", "STATELESS", "No-op (never waiting).",
 "ActivityThread attach when debuggable.", "L", "Not hit for a non-waiting launch."),

# ---------------- IActivityTaskManager ----------------
("android.app.IActivityTaskManager", "svc.atms", "getActivityClientController",
 "()Landroid/app/IActivityClientController;", "STATELESS",
 "Binder handle to our IActivityClientController.", "ActivityTaskManager.getService().getActivityClientController().",
 "L", "1 call at launch."),
("android.app.IActivityTaskManager", "svc.atms", "reportAssistContextExtras",
 "(Landroid/os/IBinder;Landroid/os/Bundle;Landroid/app/assist/AssistStructure;Landroid/app/assist/AssistContent;Landroid/net/Uri;)V",
 "STATELESS", "Accept and drop (oneway).", "AssistStructure reply path (long-press assist).",
 "L", "Interaction only; no assist in V1."),

# ---------------- IActivityClientController ----------------
("android.app.IActivityClientController", "svc.atms", "getDisplayId",
 "(Landroid/os/IBinder;)I", "STATEFUL", "0 (the only display).",
 "Activity.getDisplay / ContextImpl display attribution.", "L", "1 call at launch."),
("android.app.IActivityClientController", "svc.atms", "activityResumed",
 "(Landroid/os/IBinder;Z)V", "STATEFUL",
 "Record resumed=true for the token; this is the hook where the mini-ATMS knows the app is interactive (used by the scenario runner's 'ready' signal).",
 "ActivityThread.handleResumeActivity -> ATMS bookkeeping.", "M", "1 call at launch, after relayout."),
("android.app.IActivityClientController", "svc.atms", "activityIdle",
 "(Landroid/os/IBinder;Landroid/content/res/Configuration;Z)V", "STATEFUL",
 "Record idle; the launch gate 'first frame presented' completes here on real Android (after finishDrawing).",
 "ActivityThread.handleActivityIdle -> 'Activity idle' log / launch completion.", "M", "1 call at launch; in ACOS this is what `am start -W` would wait on."),
("android.app.IActivityClientController", "svc.atms", "setTaskDescription",
 "(Landroid/os/IBinder;Landroid/app/ActivityManager$TaskDescription;)V", "STATELESS",
 "Accept (recents bookkeeping).", "Activity.setTaskDescription.", "L", ""),
("android.app.IActivityClientController", "svc.atms", "reportSizeConfigurations",
 "(Landroid/os/IBinder;Landroid/window/SizeConfigurationBuckets;)V", "STATELESS",
 "Accept (needed for size-config change handling).", "Activity.reportSizeConfigurations during relaunch.",
 "L", "Decoration for multi-window; single-window V1."),

# ---------------- IWindowManager + IWindowSession (mini-WMS) ----------------
("android.view.IWindowManager", "svc.wms", "getCurrentAnimatorScale",
 "()F", "STATELESS", "1.0f (Settings.Global.ANIMATOR_DURATION_SCALE unset on the reference device).",
 "ValueAnimator/WindowManagerGlobal animator scale.", "L", "Measured: device-config.json says font/anim scales are default."),
("android.view.IWindowManager", "svc.wms", "useBLAST",
 "()Z", "STATELESS", "true (API 34 default).", "ViewRootImpl chooses the BLAST surface path.",
 "L", "Returning false would select the legacy path - a fidelity break."),
("android.view.IWindowManager", "svc.wms", "hasNavigationBar",
 "(I)Z", "STATELESS", "true (display.txt: NavigationBar0 present, 63px bottom inset).",
 "ViewRootImpl/insets decisions.", "L", ""),
("android.view.IWindowManager", "svc.wms", "isInTouchMode",
 "(I)Z", "STATELESS", "true (touch device; the reference interaction is touch).",
 "ViewRootImpl.isInTouchMode -> focus behaviour.", "L", ""),
("android.view.IWindowManager", "svc.wms", "openSession",
 "(Landroid/view/IWindowSessionCallback;)Landroid/view/IWindowSession;", "STATEFUL",
 "Create a session object (id=0), record the callback, return its binder.",
 "WindowManagerGlobal.getWindowSession() -> ViewRootImpl.", "M", "1 call at launch; the session is then used for every window operation."),
("android.view.IWindowManager", "svc.wms", "setInTouchMode",
 "(ZI)V", "STATELESS", "Accept; touch mode is authoritative in ACOS (always true for touch).",
 "ViewRootImpl.setInTouchMode.", "L", "Interaction only (2 calls)."),
("android.view.IWindowSession", "svc.wms", "addToDisplayAsUser",
 "(Landroid/view/IWindow;Landroid/view/WindowManager$LayoutParams;IIIILandroid/view/InputChannel;Landroid/view/InsetsState;Landroid/view/InsetsSourceControl$Array;Landroid/graphics/Rect;[F)I",
 "STATEFUL",
 "Create the window record {token, LayoutParams, frame=[0,0][1080,2337], parent=default task layer, visible=true}; create an InputChannel (socketpair + token binder) and write it into the out InputChannel argument; write InsetsState (statusBars 0,0-1080,128 visible; navigationBars 0,2337-1080,2400 visible; ime 0,2274-1080,2400 invisible; displayCutout 0,0-1080,128) and InsetsSourceControl[] with surface positions; outFrame=[0,128][1080,2337]; return ADD_OKAY(0).",
 "ViewRootImpl.setView -> window attached, input channel registered, insets dispatched.",
 "H", "The single most complex reply on the path: 4 out-parameters (InputChannel, InsetsState, InsetsSourceControl[], Rect, float[]). Values from display.txt InsetsState section."),
("android.view.IWindowSession", "svc.wms", "relayout",
 "(Landroid/view/IWindow;Landroid/view/WindowManager$LayoutParams;IIIIIILandroid/window/ClientWindowFrames;Landroid/util/MergedConfiguration;Landroid/view/SurfaceControl;Landroid/view/InsetsState;Landroid/view/InsetsSourceControl$Array;Landroid/os/Bundle;)I",
 "STATEFUL",
 "STATEFUL and frame-driven: honour width/height/flags (MATCH_PARENT/1080x2337), write ClientWindowFrames{frame=[0,128][1080,2337], displayFrame=[0,0][1080,2400]}, MergedConfiguration=current config, a NEW SurfaceControl for the app layer (from the compositor), InsetsState as above, and return relayoutResult (0, or RELAYOUT_RES_FIRST_TIME=1 for the first relayout). Must NOT block; the app will draw next frame.",
 "ViewRootImpl.relayoutWindow -> mSurface created/updated -> performTraversals.",
 "H", "Called once at launch and 7 resized-triggered times in interaction. This is the crux of 'mini-WMS': a real (small) state machine, not a canned reply."),
("android.view.IWindowSession", "svc.wms", "finishDrawing",
 "(Landroid/view/IWindow;Landroid/view/SurfaceControl$Transaction;I)V", "STATEFUL",
 "Apply the transaction to the compositor (this is the app's commit point), take the rendered buffer from the render thread, assign frame_seq/content hash, wake the agent-facing frame waiter, and schedule the frame-complete callback back to the app.",
 "ViewRootImpl.finishDrawing -> frame committed (reportDrawFinished).",
 "H", "1 call at launch, right before activityIdle. acos-render's Frame/freshness contract attaches here."),
("android.view.IWindowSession", "svc.wms", "setOnBackInvokedCallbackInfo",
 "(Landroid/view/IWindow;Landroid/window/OnBackInvokedCallbackInfo;)V", "STATEFUL",
 "Record the callback; on a back gesture/key, invoke OnBackInvokedCallback.onBackInvoked through the app looper.",
 "ViewRootImpl.setOnBackInvokedCallbackInfo (predictive back).",
 "M", "1-2 calls; V1 may instead inject KEYCODE_BACK as an input event (the reference used `input keyevent`)."),
("android.view.IWindowSession", "svc.wms", "updateRequestedVisibleTypes",
 "(Landroid/view/IWindow;I)V", "STATEFUL", "Record the requested insets types; no change unless IME appears.",
 "ViewRootImpl.updateRequestedVisibleTypes.", "L", ""),
("android.view.IWindowSession", "svc.wms", "onRectangleOnScreenRequested",
 "(Landroid/os/IBinder;Landroid/graphics/Rect;)V", "STATEFUL",
 "No-op (18 calls in interaction: TextView.bringIntoView during IME show).",
 "View.requestRectangleOnScreen (scroll to reveal the focused view).", "L",
 "High count, zero behaviour: safe to no-op only because ACOS does not scroll a real IME."),

# ---------------- IGraphicsStats ----------------
("android.view.IGraphicsStats", "svc.gfxstats", "requestBufferForProcess",
 "(Ljava/lang/String;Landroid/view/IGraphicsStatsCallback;)Landroid/os/ParcelFileDescriptor;",
 "STATEFUL",
 "Return a memfd-backed ParcelFileDescriptor sized to the ProcessStatsBuffer ABI; HardwareRenderer.nSetProcessStatsBuffer(fd) (measured) then writes frame stats into it. ACOS keeps it zeroed + updates the frame counters ACOS actually knows.",
 "HardwareRenderer -> FrameMetrics aggregation.", "M",
 "1 call at launch. A null fd breaks the renderer's stats setup."),

# ---------------- accessibility ----------------
("android.view.accessibility.IAccessibilityManager", "svc.a11y", "addClient",
 "(Landroid/view/accessibility/IAccessibilityManagerClient;I)J", "STATEFUL",
 "Record the client, return clientId=1, and push one AccessibilityManager.ClientInfo state (enabled=false, touchExploration=false) through setState on the client's looper.",
 "AccessibilityManager.getAccessibilityServiceList/enabled state.", "M",
 "Enabling a11y would add a second, large code path; keep it disabled (matches reference)."),
("android.view.accessibility.IAccessibilityManager", "svc.a11y", "getRecommendedTimeoutMillis",
 "()J", "STATELESS", "0 (no timeout extension).", "ViewConfiguration.getRecommendedTimeoutMillis.", "L", ""),
("android.view.accessibility.IAccessibilityManager", "svc.a11y", "getFocusStrokeWidth",
 "()I", "STATELESS", "0 (no a11y focus indicator).", "View.onFocusChanged drawing.", "L", ""),
("android.view.accessibility.IAccessibilityManager", "svc.a11y", "getFocusColor",
 "()I", "STATELESS", "0.", "View drawing of focus highlight.", "L", ""),

# ---------------- game / virtual device / content capture / text services ----------------
("android.app.IGameManagerService", "svc.misc", "isAngleEnabled",
 "(Ljava/lang/String;I)Z", "STATELESS", "false.", "ActivityThread/GraphicsEnvironment ANGLE check.", "L", ""),
("android.companion.virtual.IVirtualDeviceManager", "svc.misc", "getDeviceIdForDisplayId",
 "(I)I", "STATELESS", "0 (no virtual device for display 0).",
 "ContextImpl device-id attribution for a display.", "L", "2 calls at launch."),
("android.view.contentcapture.IContentCaptureManager", "svc.misc", "registerContentCaptureOptionsCallback",
 "(Ljava/lang/String;Landroid/view/contentcapture/IContentCaptureOptionsCallback;)V",
 "STATELESS", "Record; never emit options (content capture disabled).",
 "ContentCaptureManager init.", "L", ""),
("com.android.internal.textservice.ITextServicesManager", "svc.textservices", "getCurrentSpellCheckerSubtype",
 "(IZ)Landroid/view/textservice/SpellCheckerSubtype;", "STATELESS",
 "null (no spell checker installed).", "SpellCheckerSession / TextView spell check.", "L",
 "2 calls at launch, 6 in interaction."),
("com.android.internal.textservice.ITextServicesManager", "svc.textservices", "isSpellCheckerEnabled",
 "(I)Z", "STATELESS", "false.", "TextView.updateTextServices.", "L", "5 calls in interaction."),
("com.android.internal.textservice.ITextServicesManager", "svc.textservices", "getCurrentSpellChecker",
 "(ILjava/lang/String;)Landroid/view/textservice/SpellCheckerInfo;", "STATELESS",
 "null.", "SpellCheckerSession creation.", "L", ""),
("com.android.internal.textservice.ITextServicesManager", "svc.textservices", "getSpellCheckerService",
 "(ILjava/lang/String;Ljava/lang/String;Lcom/android/internal/textservice/ITextServicesSessionListener;Lcom/android/internal/textservice/ISpellCheckerSessionListener;Landroid/os/Bundle;I)V",
 "STATELESS",
 "Do not bind: either deliver onServiceConnected with a null session or throw a RemoteException-equivalent. Returning null+no callback must be tested for a hang.",
 "SpellCheckerSession.getSpellCheckerSession.", "M",
 "Interaction only; risk is a silent wait in the app."),

# ---------------- input manager ----------------
("android.hardware.input.IInputManager", "svc.input", "getVelocityTrackerStrategy",
 "()Ljava/lang/String;", "STATELESS", "\"impulse\".", "VelocityTracker native strategy selection.", "L", ""),
("android.hardware.input.IInputManager", "svc.input", "registerInputDevicesChangedListener",
 "(Landroid/hardware/input/IInputDevicesChangedListener;)V", "STATEFUL",
 "Record; emit nothing (device set is static).", "InputManager device list.", "L", ""),
("android.hardware.input.IInputManager", "svc.input", "getInputDeviceIds",
 "()[I", "STATELESS", "{0} (the synthetic touchscreen/keyboard).", "InputManager.getInputDeviceIds.", "L", ""),
("android.hardware.input.IInputManager", "svc.input", "getInputDevice",
 "(I)Landroid/view/InputDevice;", "STATELESS",
 "InputDevice: id=0, name='ACOS Touch', sources=SOURCE_TOUCHSCREEN|SOURCE_KEYBOARD, keyboardType=ALPHABETIC, vendor/product 0, motionRanges=[{axis X, -1..1080},{axis Y,-1..2400}].",
 "InputManager.getInputDevice -> ViewConfiguration/MotionEvent scale factors.", "M",
 "Motion ranges matter: the framework uses them to normalise touch coordinates."),

# ---------------- IME ----------------
("com.android.internal.view.IInputMethodManager", "svc.ime", "addClient",
 "(Lcom/android/internal/inputmethod/IInputMethodClient;Lcom/android/internal/inputmethod/IRemoteInputConnection;I)V",
 "STATEFUL", "Record the client + default remote connection (needed for the IME-focus path).",
 "InputMethodManager.init -> every IMMS call.", "M", "1 call at launch."),
("com.android.internal.view.IInputMethodManager", "svc.ime", "isImeTraceEnabled",
 "()Z", "STATELESS", "false.", "InputMethodManager tracing.", "L", ""),
("com.android.internal.view.IInputMethodManager", "svc.ime", "startInputOrWindowGainedFocus",
 "(ILcom/android/internal/inputmethod/IInputMethodClient;Landroid/os/IBinder;IIILandroid/view/inputmethod/EditorInfo;Lcom/android/internal/inputmethod/IRemoteInputConnection;Lcom/android/internal/inputmethod/IRemoteAccessibilityInputConnection;IILandroid/window/ImeOnBackInvokedDispatcher;)Lcom/android/internal/inputmethod/InputBindResult;",
 "STATEFUL",
 "V1 (headless IME): return InputBindResult with resultCode=NO_IME (3)/startInputResult=FAILED(0) and a null session; do NOT bind an IME. Documented divergence: the soft keyboard never shows, text arrives as injected key events. Faithful variant (later): return SUCCESS_WITH_IME_SESSION and deliver IInputMethodClient.onBindMethod + a session stub.",
 "InputMethodManager.startInputInner -> ViewRootImpl/TextView editing state.",
 "H", "1 call at launch, 1 in interaction. The biggest deliberate divergence on the path; must be a *declared* capability, surfaced in evidence."),
("com.android.internal.view.IInputMethodManager", "svc.ime", "reportPerceptibleAsync",
 "(Landroid/os/IBinder;Z)V", "STATELESS", "No-op (oneway).", "InsetsController perceptible-window hint.", "L",
 "1 at launch, 6 in interaction."),
("com.android.internal.view.IInputMethodManager", "svc.ime", "removeImeSurfaceFromWindowAsync",
 "(Landroid/os/IBinder;)V", "STATELESS", "No-op (oneway).", "IMMS after hide.", "L", ""),
("com.android.internal.view.IInputMethodManager", "svc.ime", "showSoftInput",
 "(Lcom/android/internal/inputmethod/IInputMethodClient;Landroid/os/IBinder;Landroid/view/inputmethod/ImeTracker$Token;IILandroid/os/ResultReceiver;I)Z",
 "STATEFUL", "false (no IME) - the ResultReceiver must still be told (RESULT_SHOWN/RESULT_UNCHANGED_HIDDEN) or the app may wait.",
 "InputMethodManager.showSoftInput.", "M", "1 call in interaction."),
("com.android.internal.view.IInputMethodManager", "svc.ime", "hideSoftInput",
 "(Lcom/android/internal/inputmethod/IInputMethodClient;Landroid/os/IBinder;Landroid/view/inputmethod/ImeTracker$Token;ILandroid/os/ResultReceiver;I)Z",
 "STATEFUL", "false + deliver the ResultReceiver result.", "InputMethodManager.hideSoftInput.", "M", "1 call in interaction."),
("com.android.internal.view.IInputMethodManager", "svc.ime", "getImeTrackerService",
 "()Lcom/android/internal/inputmethod/IImeTracker;", "STATELESS", "Binder handle to our IImeTracker stub.",
 "InputMethodManager/InsetsController IME tracking.", "L", ""),
("com.android.internal.inputmethod.IInputMethodSession", "svc.ime", "viewClicked",
 "(Z)V", "STATELESS", "No-op (oneway).", "IMMS view click notification.", "L", "1 call in interaction."),
("com.android.internal.inputmethod.IInputMethodSession", "svc.ime", "updateSelection",
 "(IIIIII)V", "STATELESS", "No-op (oneway).", "TextView selection -> IME.", "L", "11 calls in interaction."),
("com.android.internal.inputmethod.IImeTracker", "svc.ime", "onRequestShow",
 "(Ljava/lang/String;III)Landroid/view/inputmethod/ImeTracker$Token;", "STATEFUL",
 "Return a Token with a fresh monotonically increasing id and the given tag; store it.",
 "InputMethodManager.showSoftInput request accounting.", "L", ""),
("com.android.internal.inputmethod.IImeTracker", "svc.ime", "onProgress",
 "(Landroid/os/IBinder;I)V", "STATEFUL", "Record (oneway).", "IME show/hide progress.", "L", "22 calls in interaction."),
("com.android.internal.inputmethod.IImeTracker", "svc.ime", "onShown",
 "(Landroid/view/inputmethod/ImeTracker$Token;)V", "STATEFUL", "Record.", "IME shown.", "L", ""),
("com.android.internal.inputmethod.IImeTracker", "svc.ime", "onRequestHide",
 "(Ljava/lang/String;III)Landroid/view/inputmethod/ImeTracker$Token;", "STATEFUL", "Return a new Token.", "IME hide request.", "L", ""),
("com.android.internal.inputmethod.IImeTracker", "svc.ime", "onHidden",
 "(Landroid/view/inputmethod/ImeTracker$Token;)V", "STATEFUL", "Record.", "IME hidden.", "L", ""),

# ---------------- autofill / voice / audio / content service / futures ----------------
("android.view.autofill.IAutoFillManager", "svc.autofill", "addClient",
 "(Landroid/view/autofill/IAutoFillManagerClient;Landroid/content/ComponentName;ILcom/android/internal/os/IResultReceiver;)V",
 "STATEFUL", "Record the client; report disabled via the IResultReceiver.", "AutofillManager init.", "M", ""),
("android.view.autofill.IAutoFillManager", "svc.autofill", "startSession",
 "(Landroid/os/IBinder;Landroid/os/IBinder;Landroid/view/autofill/AutofillId;Landroid/graphics/Rect;Landroid/view/autofill/AutofillValue;IZILandroid/content/ComponentName;ZLcom/android/internal/os/IResultReceiver;)V",
 "STATEFUL", "Return (via the IResultReceiver) 'no session' immediately: autofill disabled.",
 "AutofillManager.startSession -> View notifyViewEntered.", "M",
 "Must not leave the app waiting on the receiver."),
("com.android.internal.app.IVoiceInteractionManagerService", "svc.misc", "notifyActivityEventChanged",
 "(Landroid/os/IBinder;I)V", "STATELESS", "No-op (oneway).", "VoiceInteractionSession activity events.", "L", ""),
("android.content.IContentService", "svc.contentservice", "registerContentObserver",
 "(Landroid/net/Uri;ZLandroid/database/IContentObserver;II)V", "STATEFUL",
 "Record (uri, observer). Emit onChange(false) only when ACOS changes the value (e.g. a scenario `settings put`), which is what makes Settings observers behave.",
 "Settings$GenerationTracker / DeviceConfig observers.", "M", "1 call in interaction."),
("android.media.IAudioService", "svc.audio", "areNavigationRepeatSoundEffectsEnabled",
 "()Z", "STATELESS", "false.", "View.playSoundEffect gating.", "L", ""),
("android.media.IAudioService", "svc.audio", "playSoundEffect",
 "(II)V", "STATELESS", "Accept and drop (oneway). No audio device in V1.",
 "View/AudioManager on click.", "L",
 "Deliberate divergence: no audible feedback. Must be declared, not faked as success."),
("com.android.internal.infra.IAndroidFuture", "svc.futures", "complete",
 "(Lcom/android/internal/infra/AndroidFuture;)V", "STATELESS",
 "No-op on the service side: ACOS never blocks on an AndroidFuture. (In-process there is no waiting server thread.)",
 "TextServices/IMMS async plumbing.", "L", "8 calls in interaction."),

# ---------------- IContentProvider (hand-written proxy; MISSED by the trace parser) ----------------
("android.content.IContentProvider", "svc.provider", "call",
 "(Landroid/content/AttributionSource;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Landroid/os/Bundle;)Landroid/os/Bundle;",
 "STATEFUL",
 "Settings semantics: method GET_global/GET_secure/GET_system with arg=key -> Bundle{value=<String or null>} plus, for the first call per namespace, a ParcelFileDescriptor backed by a 4-byte MemoryIntArray as the generation counter (Settings$GenerationTracker uses MemoryIntArray.nativeOpen/nativeGet, both measured). PUT_* updates the in-memory table and bumps the generation.",
 "Settings$NameValueCache.getStringForUser / Settings$GenerationTracker -> DeviceConfig, animation scale, a11y flags.",
 "H",
 "Counts NOT in the TSV: the parser only matches *$Stub$Proxy classes; ContentProviderProxy is hand-written (measured: 5 launch / 4 interaction calls, which reconciles 73-68 and 106-102 transactNative entries exactly)."),
("android.content.IContentProvider", "svc.provider", "query",
 "(Landroid/content/AttributionSource;Landroid/net/Uri;[Ljava/lang/String;Landroid/os/Bundle;Landroid/os/ICancellationSignal;)Landroid/database/Cursor;",
 "STATEFUL", "Empty cursor / null (no provider rows needed for V1).",
 "ContentResolver.query if an app/framework path queries Settings directly.", "M",
 "Not observed in either trace; keep as an explicit `unsupported-query` rather than a fake empty table."),
]

# Counts for transactions the TSV parser cannot see (hand-written proxies).
# MEASURED from data/methods_hello_views_{launch,interaction}.tsv:
#   android.content.ContentProviderProxy.call  launch 5, interaction 4
#   (these reconcile 73-68 and 106-102 transactNative entries exactly)
MANUAL_COUNTS = {("android.content.IContentProvider", "call"): {"launch": 5, "interaction": 4}}

# Registry of incoming (system -> app) transports/handlers we must synthesise.
INCOMING = [
("android.app.IApplicationThread", "svc.atms", "bindApplication", "STATEFUL",
 "The mini-ATMS's entry into the app: must be delivered after attachApplication. Carries AppBindData{ApplicationInfo, provider list, instrumentationName, profile mode, Configuration, CompatibilityInfo, base package, restricted backup dir, app class, {...}}. Materialise the Java AppBindData and call ApplicationThread.bindApplication directly (bypassing Parcel) OR marshal the full AIDL argument list; recommend direct VM call, verified equal to the marshalled form by a differential test.",
 "ActivityThread.handleBindApplication -> Application creation -> ContentProviders -> Application.onCreate.",
 "H", "Pre-profile in the ART trace (REPORT: at 27.7 ms) so it is absent from binder_calls_launch.tsv."),
("android.app.IApplicationThread", "svc.atms", "scheduleTransaction", "STATEFUL",
 "ClientTransaction with LaunchActivityItem/ResumeActivityItem/PauseActivityItem/StopActivityItem/DestroyActivityItem. Delivery order is a real state machine: launch(create+start) -> resume -> idle -> pause -> stop -> destroy. must be posted to the app's main looper, never invoked inline.",
 "ActivityThread$ApplicationThread.scheduleTransaction -> TransactionExecutor -> Activity.performCreate/onResume.",
 "H", "Not in the measured 51 (pre-profile / merged into launch); the interaction trace shows the app's own lifecycle instead."),
("android.app.IApplicationThread", "svc.atms", "scheduleEnterAnimationComplete", "STATEFUL",
 "Post the message after the first finishDrawing (real Android base sets a 120ms*scale timeout). No timeout is needed in ACOS because the frame is deterministic.",
 "ActivityThread.handleEnterAnimationComplete -> ViewRootImpl reports 'animation complete'.", "M", "Measured 1x at launch."),
("android.app.IApplicationThread", "svc.atms", "requestAssistContextExtras", "STATEFUL",
 "Only after an assist request; no-op reply in V1.", "AssistStructure capture.", "L", "Measured 1x in interaction."),
("android.app.IApplicationThread", "svc.atms", "profilerControl", "STATEFUL",
 "Start/stop the ACOS method tracer (used by the trace tooling).", "ActivityThread profiler plumbing.", "M", "Measured 1x in interaction (am profile stop)."),
("android.view.IWindow", "svc.wms", "resized", "STATEFUL",
 "ClientWindowFrames + Configuration after a relayout; post to the UI looper.", "ViewRootImpl.dispatchResized -> performTraversals.", "H", "13 onTransacts / 7 resized in interaction."),
("android.view.IWindow", "svc.wms", "insetsControlChanged", "STATEFUL",
 "InsetsState + InsetsSourceControl[]; post to the UI looper.",
 "ViewRootImpl.handleInsetsControlChanged -> InsetsController.", "M", "2 at launch, 4 in interaction."),
("android.view.IWindow", "svc.wms", "showInsets", "STATEFUL",
 "InsetsFlags + fromIme; post to the looper.", "InsetsController.show -> status/nav bar appearance.", "L", "1 in interaction."),
("android.view.IWindow", "svc.wms", "hideInsets", "STATEFUL", "As showInsets.", "InsetsController.hide.", "L", "1 in interaction."),
("android.view.IWindow", "svc.wms", "dispatchAppVisibility", "STATEFUL",
 "Visibility; drives onStart/onStop ordering for multi-window cases.", "ViewRootImpl visibility state.", "M",
 "Not observed in the traces (single-window launch) but part of the contract."),
("android.view.IWindow", "svc.wms", "windowFocusChanged", "STATEFUL",
 "Focus flag; must be true for the first window so the button click and IME focus behave as on the reference.",
 "ViewRootImpl focus -> View.onWindowFocusChanged.", "M", "Not observed in the trace; required for input/IME ordering."),
("com.android.internal.inputmethod.IInputMethodClient", "svc.ime", "setActive", "STATEFUL",
 "active flag + session; post to the looper.", "InputMethodManager.setActive.", "L", "1 at launch."),
("com.android.internal.inputmethod.IInputMethodClient", "svc.ime", "onBindMethod", "STATEFUL",
 "InputBindResult; the app adopts the session.", "InputMethodManager.onBindMethod.", "M", "1 at launch."),
("com.android.internal.inputmethod.IInputMethodClient", "svc.ime", "reportFullscreenMode", "STATEFUL",
 "Fullscreen flag.", "IMMS fullscreen bookkeeping.", "L", "1 at launch."),
("com.android.internal.inputmethod.IRemoteInputConnection", "svc.ime", "getSurroundingText",
 "(IIII)Landroid/view/inputmethod/SurroundingText;", "STATEFUL",
 "Answer from the app's EditorInfo/TextKeyListener state (the Java side handles it; we only deliver the call).",
 "RemoteInputConnectionImpl -> text editing state.", "M", "8 in interaction."),
("com.android.internal.inputmethod.IRemoteInputConnection", "svc.ime", "setComposingRegion",
 "(II)V", "STATEFUL", "Deliver (no-op if no IME).", "Text editing.", "L", "5 in interaction."),
("com.android.internal.inputmethod.IRemoteInputConnection", "svc.ime", "clearMetaKeyStates",
 "(I)V", "STATEFUL", "Deliver.", "Text editing.", "L", "1 in interaction."),
("com.android.internal.inputmethod.IRemoteInputConnection", "svc.ime", "finishComposingText",
 "()V", "STATEFUL", "Deliver.", "Text editing.", "L", "1 in interaction."),
("com.android.internal.os.IResultReceiver", "svc.atms", "send",
 "(ILandroid/os/Bundle;)V", "STATEFUL",
 "Deliver on the caller's looper; used by SyncResultReceiver (spell check, autofill, show/hide IME) - a missing reply is a hang.",
 "SyncResultReceiver.resultData.", "M", "4 onTransacts in interaction."),
("android.view.autofill.IAutoFillManagerClient", "svc.autofill", "setSessionFinished",
 "(ILjava/util/List;)V", "STATEFUL", "Deliver (autofill disabled -> session never starts).",
 "AutofillManager client callback.", "L", "1 in interaction."),
("com.android.internal.textservice.ITextServicesSessionListener", "svc.textservices", "onServiceConnected",
 "(Lcom/android/internal/textservice/ISpellCheckerSession;)V", "STATEFUL",
 "Deliver only if we bind a spell-checker service; V1 answers 'no service' instead. Must be tested against the app's timeout path.",
 "SpellCheckerSession.SpellCheckerSessionListener.", "M", "1 onTransact in interaction."),
("android.hardware.display.IDisplayManagerCallback", "svc.display", "onDisplayEvent",
 "(II)V", "STATEFUL", "Emit only on real display change (rotation/resize scenario).",
 "DisplayManagerGlobal -> DisplayManager.DisplayListener.", "L", "Not observed (no display change)."),
("android.hardware.input.IInputDevicesChangedListener", "svc.input", "onInputDevicesChanged",
 "([I)V", "STATEFUL", "Emit once with {(0,0)} if the app enumerates; static device set.",
 "InputManager.InputDeviceListener.", "L", "Not observed."),
("android.view.accessibility.IAccessibilityManagerClient", "svc.a11y", "setState",
 "(Landroid/view/accessibility/AccessibilityManager$ClientInfo;)V", "STATEFUL",
 "Deliver the disabled state once after addClient.", "AccessibilityManager state.", "L", "Not observed (disabled)."),
("android.view.IWindowSessionCallback", "svc.wms", "onAnimatorScaleChanged",
 "(F)V", "STATEFUL", "Deliver only if the animator scale changes (never in V1).",
 "ViewRootImpl animator scale.", "L", "Not observed."),
]

def main():
    cols = ["interface", "direction", "owner", "method", "signature", "launch_calls",
            "interaction_calls", "state", "reply_or_delivery", "consumer", "risk", "notes"]
    rows = []
    used = set()
    for (iface, owner, meth, sig, state, reply, consumer, risk, notes) in OUT:
        c = counts.get((iface, meth)) or MANUAL_COUNTS.get((iface, meth), {"launch": 0, "interaction": 0})
        used.add((iface, meth))
        rows.append({"interface": iface, "direction": "app->system", "owner": owner,
                     "method": meth, "signature": sig,
                     "launch_calls": c["launch"], "interaction_calls": c["interaction"],
                     "state": state, "reply_or_delivery": reply, "consumer": consumer,
                     "risk": risk, "notes": notes})
    for row in INCOMING:
        if len(row) == 9:
            iface, owner, meth, sig, state, reply, consumer, risk, notes = row
        else:
            iface, owner, meth, state, reply, consumer, risk, notes = row
            sig = ""
        rows.append({"interface": iface, "direction": "system->app", "owner": owner,
                     "method": meth, "signature": sig, "launch_calls": "",
                     "interaction_calls": "", "state": state,
                     "reply_or_delivery": reply, "consumer": consumer,
                     "risk": risk, "notes": notes})
    dst = os.path.join(HERE, "services.tsv")
    with open(dst, "w", newline="") as fh:
        w = csv.DictWriter(fh, fieldnames=cols, delimiter="\t",
                           lineterminator="\n", quoting=csv.QUOTE_MINIMAL)
        w.writeheader()
        for r in rows:
            w.writerow(r)
    print("wrote", dst, len(rows), "rows")
    missing = set(counts) - used
    print("measured transactions with no catalogue row:", sorted(missing))
    extra = {k for k in used if k not in counts}
    print("catalogue rows not in the measured trace:", len(extra))
    import collections
    print("by risk:", dict(collections.Counter(r["risk"] for r in rows)))
    print("by direction:", dict(collections.Counter(r["direction"] for r in rows)))

if __name__ == "__main__":
    main()
