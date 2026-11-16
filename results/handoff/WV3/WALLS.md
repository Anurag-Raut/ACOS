# WV3 — wall map (WebView track, guest side, session 3)

Branch `wv3` (from `wv2` @ 6937d181). Every row is a run whose logcat line is
quoted; evidence is under `results/handoff/WV3/evidence/`. Two ART runs in one
private runtime dir (`$WT/.cache/wv3-run`), each one acquisition of the shared
lock, neither a benchmark.

| # | wall | before WV3 | now |
|---|---|---|---|
| 1 | uid guard | satisfied | unchanged |
| 2 | `hasSystemFeature("android.software.webview")` | `true` | unchanged |
| 3 | `webviewupdate.waitForAndGetProvider` reply encoding | fixed in WV2 | unchanged |
| 4 | `WebViewFactory` reply/`PackageInfo` decode | fixed in WV2 | unchanged |
| 5 | `IActivityManager.addPackageDependency` (code 96) | `UnsupportedOperationException: unsupported Android API: android.app.IActivityManager code 96` | **CLOSED** — AMS records the dependency and replies void (§1) |
| 6 | `verifyPackageInfo`: the provider package must declare its WebView native library | `MissingWebViewPackageException: Tried to load an invalid WebView provider: com.android.webview` (`WebViewFactory.java:404`) | **CLOSED** — the provider manifest declares `com.android.webview.WebViewLibrary`; the load itself is AOSP's honest no-op (§2) |
| 6b | `initialApplication.createApplicationContext(ai, INCLUDE_CODE\|IGNORE_SECURITY)` for a package the process did not launch | not reached in WV2 | **NO WALL** — worked first try on MPROC1's `extra_apks` + PMS meta-data support (§3) |
| 7 | provider `ClassLoader` + `WebViewChromiumFactoryProviderForT.create(WebViewDelegate)` | not reached | **CLOSED** — `ACOSWvProvider: create(WebViewDelegate present)` (§3) |
| 8 | the provider's transport: `LocalSocket.connect("/run/acos/webview.sock")` | not reached | **BLOCKED (new)**: `UnsatisfiedLinkError: No implementation found for void android.net.LocalSocketImpl.connectLocal(java.io.FileDescriptor, java.lang.String, int)` (§4) |
| 9 | `new WebView(...)` inside the fixture | not reached | not reached (same wall: the provider opens the transport in `createWebView`) |

The session table is the oracle: one run per change, same scenario
(`.cache/wv3-run/scenario.yaml`, a copy of WV2's
`results/handoff/WV2/webview-probe-provider.yaml`), same fixture.

## 1. Wall 5: `IActivityManager.addPackageDependency`

`crates/acos-services/src/ams.rs`: a `dispatch` arm plus
`AmsState::add_package_dependency`, recording into a new
`AmsState.package_dependencies` set (`ProcessRecord.pkgDeps` on a device) and
replying void. The `acos-aidl` table already decoded the right shape from the
image, so no table change was needed — the missing piece was the AMS arm.

**The brief's `addPackageDependency(String,int)` is not the API-34 wire shape.**
The image dex is authoritative:

```
method IActivityManager$Stub$Proxy;->addPackageDependency(Ljava/lang/String;)V
  0011: invoke-virtual {v0, v6}, Landroid/os/Parcel;->writeString:(Ljava/lang/String;)V
  0016: const/16 v3, #int 96
  0019: invoke-interface {v2, v3, v0, v1, v4}, Landroid/os/IBinder;->transact(...)
  001d: invoke-virtual {v1}, Landroid/os/Parcel;->readException:()V
```

`onTransact` reads one `readString()`, calls `enforceNoDataAvail()`, then
`writeNoException()`. Both callers in the image (`LoadedApk`, and
`WebViewFactory` via `ActivityManager.getService()`) pass one `String`, and the
AOSP source cache agrees (`base/core/java/android/android/webkit/WebViewFactory.java:463`).
The `(String, int)` overload belongs to a later platform; implementing it here
would have desynchronised the parcel. Full disassembly dump:
`evidence/wall5-aidl-shape.txt`.

Regression test (fails before, passes after — measured both ways by disabling
only the match arm): `ams::tests::add_package_dependency_is_recorded_and_replies_void`
in `crates/acos-services/src/ams.rs`. It pins the AIDL shape (code 96, one
`Str16`, empty reply, not oneway) *and* the behaviour (recorded, empty reply,
idempotent, empty name refused). Output: `evidence/wall5-test-before-after.txt`.

## 2. Wall 6: the framework requires a `WebViewLibrary` declaration

`verifyPackageInfo` is not just a signature/version check — the image's own
bytecode requires the native-library meta-data:

```
method WebViewFactory;->verifyPackageInfo(...)
  0018: iget-object v0, v7, ApplicationInfo;.metaData
  001a: invoke-static {v0}, WebViewFactory;->getWebViewLibrary:(ApplicationInfo;)String
  001e: if-eqz v0, 0x0033                 # null -> throw
  0033: new-instance ... "Tried to load an invalid WebView provider: "
method WebViewFactory;->getWebViewLibrary(ApplicationInfo;)String
  0006: const-string v1, "com.android.webview.WebViewLibrary"
  0008: invoke-virtual {v0, v1}, Landroid/os/Bundle;->getString(...)
```

WV2's provider manifest deliberately omitted that key ("pure Java provider, no
`.so`"), which is not a legal provider on this platform. The key is now
declared (`fixtures/apps/webview-provider/AndroidManifest.xml`) **and the load
stays an honest no-op**, because that is exactly AOSP's own behaviour for a
process that did not reserve address space in the zygote:

```
method WebViewLibraryLoader;->loadNativeLibrary(ClassLoader, String)I
  0000: sget-boolean v0, WebViewLibraryLoader;.sAddressSpaceReserved
  0002: if-nez v0, 0x000d
  0004: ... Log.e("can't load with relro file; address space not reserved")
  000b: const/4 v0, #int 2                 # LIBLOAD_ADDRESS_SPACE_NOT_RESERVED
  000c: return v0
```

and `WebViewFactory.getProviderClass` discards that return value. ACOS's guest
is not the zygote and never calls `WebViewFactory.prepareWebViewInZygote`, so no
`.so` is shipped and none is loaded — confirmed in-session:
`E WebViewLibraryLoader: can't load with relro file; address space not reserved`
(`evidence/ev2-logcat.txt` line 86), and the very next line is the provider's
`create(...)`.

Fails-before evidence: `evidence/ev1-logcat.txt` lines 82–87 (same scenario,
old manifest) — `Tried to load an invalid WebView provider: com.android.webview`
at `verifyPackageInfo(WebViewFactory.java:404)`.

## 3. Wall 6b/7: context + classloader + `create(...)` — passed unmodified

No foundation change was needed for either, which is the first time this track
can say that: `createApplicationContext` for a package the process did not
launch is MPROC1's `extra_apks` seam (staged APK, per-package `ApplicationInfo`,
`PathClassLoader` over the guest `sourceDir`, `ResourcesManager`/`AssetManager`
over the same APK) plus PMS meta-data parsing, and it worked on the first run:

```
82 I WebViewFactory: Loading com.android.webview version 1.0 (code 1)
83 W ziparchive: Unable to open '/data/app/webview-provider.dm': No such file or directory
85 D nativeloader: Configuring clns-3 for other apk /data/app/webview-provider.apk. target_sdk_version=34, ...
86 E WebViewLibraryLoader: can't load with relro file; address space not reserved
87 I ACOSWvProvider: create(WebViewDelegate present)
```

Line 82 is printed *after* `createApplicationContext` returned (it logs
`sPackageInfo`), 85 is the provider's own classloader being configured by
`nativeloader`, and 87 is the framework's reflective
`providerClass.getMethod("create", WebViewDelegate.class).invoke(...)` — wall 7
in the exact form the cold-start order predicts. `evidence/ev2-provider-load.txt`
is the excerpt; the whole log is `evidence/ev2-logcat.txt`.

## 4. Wall 8: the provider's transport needs an AF_UNIX connect native

`ACOSWvProvider.create(...)` runs, the WebView constructor then calls
`getProvider().createWebView(this, ...)`, the provider opens the session
transport (`WIRE.md` §0, one connect per session, `hello` first), and the guest
has no `LocalSocketImpl.connectLocal`:

```
87 I ACOSWvProvider: create(WebViewDelegate present)
88 E : No implementation found for void android.net.LocalSocketImpl.connectLocal(
        java.io.FileDescriptor, java.lang.String, int)
     (tried Java_android_net_LocalSocketImpl_connectLocal and
      Java_android_net_LocalSocketImpl_connectLocal__Ljava_io_FileDescriptor_2Ljava_lang_String_2I)
89 I WVProbe: WebView FAILED: java.lang.UnsatisfiedLinkError: ...
     at android.net.LocalSocketImpl.connectLocal(Native Method)
     at android.net.LocalSocketImpl.connect(LocalSocketImpl.java:259)
     at android.net.LocalSocket.connect(LocalSocket.java:162)
     at com.android.webview.chromium.AcosSessionSocket.open(AcosSessionSocket.java:30)
```

So the provider **reaches the socket call on the WIRE path**
(`/run/acos/webview.sock`, `Namespace.FILESYSTEM`) and is stopped by a VM
capability, not by provider code: `crates/acos-vm` implements `socketpair(AF_UNIX)`
(`Vfs::socketpair`) but no `connect` to an `AF_UNIX` path, and there is no
guest↔host socket mapping for `/run/**` in the VFS (the session VFS maps
`/data`, the boot tree and the app APK; `/run` is not a guest-visible mount).

This is the wall the orchestrator asked to be reported rather than worked
around. Two things are missing, and they are **not** interchangeable:

1. **A VM/native capability** (`LocalSocketImpl.connectLocal`): create an
   `AF_UNIX` socket in the host process the guest runs in, connect it to the
   host path that the VFS maps the guest path to, and register the connected fd
   in the `Vfs` fd table (the existing `Source::File` slots already carry host
   sockets, so `read`/`write`/`poll`/`close` would work unchanged). The honest
   failure when the mapping or the socket is absent is an `IOException` with
   the real errno — never a fake connected transport.
2. **A session socket for it to connect to** (`WIRE.md` §0: "ACOS creates it at
   a host path it owns and bind-mounts it read-write into the guest"). Nothing
   in ACOS creates the WebView session socket today, and the broker
   (`crates/acos-webview`, the `wvb` branch's `wire.rs`) is **not** in this
   tree: `git ls-tree -r wvb` carries only `results/handoff/WVB/WIRE.md`, so
   there is no broker binary to `git merge wvb` and no engine to answer
   `hello`. A test-only listener could demonstrate the handshake, but it could
   not demonstrate a page or a frame, and it would sit on the seam the host
   broker owns.

Design-wise this is the seam ADR-0031 already describes (the socket fd is the
only way out of the guest); the work is a VM native plus the session-socket
creation in `acos-art`'s launch path, both host/foundation surface. Sequencing
note for the orchestrator: the VM native alone changes the failure from
`UnsatisfiedLinkError` to `ENGINE_UNAVAILABLE` (`AcosSessionSocket.open` maps a
missing socket to a structured `IllegalStateException`), and the WebView still
cannot construct until a broker listener exists at the mapped host path —
so the two should land together, with the socket creation owned by the host
side (WVB) and the connect native by the VM.

## 5. What did **not** need a decision

* The AOSP source cache is still newer than the image in one more place and was
  ignored accordingly: the brief's `addPackageDependency(String,int)` signature.
  The image dex won, and the source cache's own `WebViewFactory.java` call site
  happens to agree with the image.
* `PackageInfo`/`ApplicationInfo` meta-data: PMS already parses
  `<application><meta-data>` from an extra APK and emits it only under
  `GET_META_DATA` (which `WebViewFactory` requests), so wall 6 needed no PMS
  change.
* `AssetManager.addAssetPathAsSharedLibrary` (provider APK as a shared library)
  and `getClassLoader` both worked; no `nativeloader` wall for a second APK
  classloader.
