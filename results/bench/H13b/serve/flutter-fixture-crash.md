# H13C task 1: `acos-counter.apk` (the 47 MB Flutter fixture) aborts in serve mode on BOTH arms

Probe: `acos serve --engine art` + `acos launch fixtures/build/apps/acos-counter.apk --engine art --keep`
with the real `XDG_RUNTIME_DIR=/run/user/1000`. Main binaries (main 3e11d9c) and
branch binaries fail identically (`GUEST_EXITED`, child exit 134). The plain
counter fixture `counter.apk` -- the one every scenario and tools/bench/acos_bench.py
uses -- passes on both. No scenario uses acos-counter.apk (`grep -rl flutter scenarios/` is empty).

## main 3e11d9c binaries (`flutter-main`)

```json
{
  "status": "error",
  "code": "GUEST_EXITED",
  "message": "acos-art: GUEST_EXITED: the guest ended before its first live frame (status exit status: 134)",
  "session_exit": null
}
```

session-exit.json excerpt:

```json
{
  "code": "GUEST_KILLED_BY_SIGNAL",
  "outcome": "crash",
  "stage": "teardown-evidence",
  "guest_exit": null,
  "guest_signal": 6,
  "activity_resumed": false,
  "application_created": true
}
```

native-crash.txt:

```
ACOSCRASH {"code":-6,"fault_addr":"0x0","pid":2,"signal":6,"source":"acos-shim","thread":"1.raster","tid":19}
*** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***
Fatal signal 6 (SIGABRT), code -6 (SI_TKILL)
OS: Linux 7.2.11-arch1-1 (x86_64)
Cmdline: <unset>
Thread: 19 "1.raster"
Registers:
    rax: 0x0000000000000000    rbx: 0x00007f2c0b763daf    rcx: 0x00007f2e9c2533f4    rdx: 0x0000000000000006
    rdi: 0x0000000000000002    rsi: 0x0000000000000013    rbp: 0x00007f2c0b762e1d    rsp: 0x00007f2bc85faca0
    r8 : 0x00007f2bc85fad78    r9 : 0x00007f2c0b763daf    r10: 0x0000000000000006    r11: 0x0000000000000206
    r12: 0x0000000000000000    r13: 0x0000000000000011    r14: 0x00000da51e65292a    r15: 0x00007f2c0b794878
    rip: 0x00007f2e9c2533f4    eflags: 0x00000206 [ PF IF ]
     cs: 0x00000033     gs: 0x00000000     fs: 0x00000000

Backtrace:
	#00 pc 000013f4  /data/shim/libartshim.so (__android_log_assert+452) (BuildId: 795647440e1146d9b73fe7ac7799359e8a564843)
```

logd tail (abort):

```
E/GeneratedPluginsRegister: Tried to automatically register plugins with FlutterEngine (io.flutter.embedding.engine.FlutterEngine@1447266) but could not find or invoke the GeneratedPluginRegistrant.
E/GeneratedPluginsRegister: Received exception while registering
java.lang.ClassNotFoundException: io.flutter.plugins.GeneratedPluginRegistrant
	at java.lang.Class.classForName(Native Method)
	at java.lang.Class.forName(Class.java:536)
	at java.lang.Class.forName(Class.java:467)
	at io.flutter.embedding.engine.plugins.util.GeneratedPluginRegister.registerGeneratedPlugins(GeneratedPluginRegister.java:77)
	at io.flutter.embedding.android.FlutterActivity.configureFlutterEngine(FlutterActivity.java:1355)
	at io.flutter.embedding.android.FlutterActivityAndFragmentDelegate.onAttach(FlutterActivityAndFragmentDelegate.java:228)
	at io.flutter.embedding.android.FlutterActivity.onCreate(FlutterActivity.java:645)
	at android.app.Activity.performCreate(Activity.java:8595)
	at android.app.Activity.performCreate(Activity.java:8573)
	at android.app.Instrumentation.callActivityOnCreate(Instrumentation.java:1456)
	at android.app.ActivityThread.performLaunchActivity(ActivityThread.java:3764)
	at android.app.ActivityThread.handleLaunchActivity(ActivityThread.java:3922)
	at android.app.servertransaction.LaunchActivityItem.execute(LaunchActivityItem.java:103)
	at android.app.servertransaction.TransactionExecutor.executeCallbacks(TransactionExecutor.java:139)
	at android.app.servertransaction.TransactionExecutor.execute(TransactionExecutor.java:96)
	at android.app.ActivityThread$H.handleMessage(ActivityThread.java:2443)
	at android.os.Handler.dispatchMessage(Handler.java:106)
	at android.os.Looper.loopOnce(Looper.java:205)
	at android.os.Looper.loop(Looper.java:294)
	at dev.acos.art.LaunchMain.run(LaunchMain.java:412)
	at dev.acos.art.LaunchMain.main(LaunchMain.java:204)
Caused by: java.lang.ClassNotFoundException: io.flutter.plugins.GeneratedPluginRegistrant
	... 21 more

E/logdw: dev.acos.fixtures.fluttercounter.MainActivity
E/logdw: dev.acos.fixtures.fluttercounter.MainActivity
E/logdw: dev.acos.fixtures.fluttercounter.MainActivity
D/CompatibilityChangeReporter: Compat change id reported: 237531167; UID 1000; state: ENABLED
E/logdw: *** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***
E/logdw: Build fingerprint: 'unknown'
E/logdw: Revision: 'unknown'
E/logdw: ABI: 'x86_64'
E/logdw: Timestamp: 2026-11-14 10:04:54.547478684+0000
E/logdw: Process uptime: 1s
E/logdw: Cmdline: /system/apex/com.android.art/bin/dalvikvm64 -cp /data/app/launch-guest/classes.dex dev.acos.art.LaunchMain /data/app/acos-counter.apk dev.acos.fixtures.fluttercounter /data/out/launch activity-resumed dev.acos.fixtures.fluttercounter.MainActivity 16974401 /data/overlay/PixelConfigOverlayCommon.idmap 2130771968 0 277364292 manifest
E/logdw: pid: 2, tid: 19, name: 1.raster  >>> /system/apex/com.android.art/bin/dalvikvm64 <<<
E/logdw: uid: 1000
E/logdw: signal 0 (SIGABRT), code -6 (SI_TKILL), fault addr --------
E/logdw: rax 0000000000000000  rbx 00007f2c0b763daf  rcx 00007f2e9c2533f4  rdx 0000000000000006
E/logdw: r8  00007f2bc85fad78  r9  00007f2c0b763daf  r10 0000000000000006  r11 0000000000000206
```

## branch perf-h13b-shared-image binaries (`flutter-branch`)

```json
{
  "status": "error",
  "code": "GUEST_EXITED",
  "message": "acos-art: GUEST_EXITED: the guest ended before its first live frame (status exit status: 134)",
  "session_exit": null
}
```

session-exit.json excerpt:

```json
{
  "code": "GUEST_KILLED_BY_SIGNAL",
  "outcome": "crash",
  "stage": "teardown-evidence",
  "guest_exit": null,
  "guest_signal": 6,
  "activity_resumed": false,
  "application_created": true
}
```

native-crash.txt:

```
ACOSCRASH {"code":-6,"fault_addr":"0x0","pid":2,"signal":6,"source":"acos-shim","thread":"1.raster","tid":19}
*** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***
Fatal signal 6 (SIGABRT), code -6 (SI_TKILL)
OS: Linux 7.2.11-arch1-1 (x86_64)
Cmdline: <unset>
Thread: 19 "1.raster"
Registers:
    rax: 0x0000000000000000    rbx: 0x00007fe883aa9daf    rcx: 0x00007feb3750a3f4    rdx: 0x0000000000000006
    rdi: 0x0000000000000002    rsi: 0x0000000000000013    rbp: 0x00007fe883aa8e1d    rsp: 0x00007fe8635faca0
    r8 : 0x00007fe8635fad78    r9 : 0x00007fe883aa9daf    r10: 0x0000000000000006    r11: 0x0000000000000206
    r12: 0x0000000000000000    r13: 0x0000000000000011    r14: 0x00000da56a0bf352    r15: 0x00007fe883ada878
    rip: 0x00007feb3750a3f4    eflags: 0x00000206 [ PF IF ]
     cs: 0x00000033     gs: 0x00000000     fs: 0x00000000

Backtrace:
	#00 pc 000013f4  /data/shim/libartshim.so (__android_log_assert+452) (BuildId: 795647440e1146d9b73fe7ac7799359e8a564843)
```

logd tail (abort):

```
E/GeneratedPluginsRegister: Tried to automatically register plugins with FlutterEngine (io.flutter.embedding.engine.FlutterEngine@1d93384) but could not find or invoke the GeneratedPluginRegistrant.
E/GeneratedPluginsRegister: Received exception while registering
java.lang.ClassNotFoundException: io.flutter.plugins.GeneratedPluginRegistrant
	at java.lang.Class.classForName(Native Method)
	at java.lang.Class.forName(Class.java:536)
	at java.lang.Class.forName(Class.java:467)
	at io.flutter.embedding.engine.plugins.util.GeneratedPluginRegister.registerGeneratedPlugins(GeneratedPluginRegister.java:77)
	at io.flutter.embedding.android.FlutterActivity.configureFlutterEngine(FlutterActivity.java:1355)
	at io.flutter.embedding.android.FlutterActivityAndFragmentDelegate.onAttach(FlutterActivityAndFragmentDelegate.java:228)
	at io.flutter.embedding.android.FlutterActivity.onCreate(FlutterActivity.java:645)
	at android.app.Activity.performCreate(Activity.java:8595)
	at android.app.Activity.performCreate(Activity.java:8573)
	at android.app.Instrumentation.callActivityOnCreate(Instrumentation.java:1456)
	at android.app.ActivityThread.performLaunchActivity(ActivityThread.java:3764)
	at android.app.ActivityThread.handleLaunchActivity(ActivityThread.java:3922)
	at android.app.servertransaction.LaunchActivityItem.execute(LaunchActivityItem.java:103)
	at android.app.servertransaction.TransactionExecutor.executeCallbacks(TransactionExecutor.java:139)
	at android.app.servertransaction.TransactionExecutor.execute(TransactionExecutor.java:96)
	at android.app.ActivityThread$H.handleMessage(ActivityThread.java:2443)
	at android.os.Handler.dispatchMessage(Handler.java:106)
	at android.os.Looper.loopOnce(Looper.java:205)
	at android.os.Looper.loop(Looper.java:294)
	at dev.acos.art.LaunchMain.run(LaunchMain.java:412)
	at dev.acos.art.LaunchMain.main(LaunchMain.java:204)
Caused by: java.lang.ClassNotFoundException: io.flutter.plugins.GeneratedPluginRegistrant
	... 21 more

E/logdw: dev.acos.fixtures.fluttercounter.MainActivity
E/logdw: dev.acos.fixtures.fluttercounter.MainActivity
E/logdw: dev.acos.fixtures.fluttercounter.MainActivity
D/CompatibilityChangeReporter: Compat change id reported: 237531167; UID 1000; state: ENABLED
E/logdw: *** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***
E/logdw: Build fingerprint: 'unknown'
E/logdw: Revision: 'unknown'
E/logdw: ABI: 'x86_64'
E/logdw: Timestamp: 2026-11-14 10:04:55.816847891+0000
E/logdw: Process uptime: 2s
E/logdw: Cmdline: /system/apex/com.android.art/bin/dalvikvm64 -Ximage:/data/img/boot.art -Xnorelocate -cp /data/app/launch-guest/classes.dex dev.acos.art.LaunchMain /data/app/acos-counter.apk dev.acos.fixtures.fluttercounter /data/out/launch activity-resumed dev.acos.fixtures.fluttercounter.MainActivity 16974401 /data/overlay/PixelConfigOverlayCommon.idmap 2130771968 0 277364292 manifest
E/logdw: pid: 2, tid: 19, name: 1.raster  >>> /system/apex/com.android.art/bin/dalvikvm64 <<<
E/logdw: uid: 1000
E/logdw: signal 0 (SIGABRT), code -6 (SI_TKILL), fault addr --------
E/logdw: rax 0000000000000000  rbx 00007fe883aa9daf  rcx 00007feb3750a3f4  rdx 0000000000000006
E/logdw: r8  00007fe8635fad78  r9  00007fe883aa9daf  r10 0000000000000006  r11 0000000000000206
```

The aborting thread is `1.raster`: the Flutter engine's own `__android_log_assert` path.
The ACOS shim interposes `__android_log_assert` (artshim.c:248) so the message reaches
logcat; the shim frame is the interposer, not the caller. `counter.apk` (native Java View)
has no raster thread and is unaffected -- see main-counter/branch-{cold,hit} evidence.
