# E2D — Can real Android **framework** Java run on real ART with no Android boot? (approach D)

**Answer: yes for `Bundle`, `Parcel`, `Looper`/`Handler`/`MessageQueue`, binder *plumbing*
(`ProcessState`), `AssetManager`/`ApkAssets`, `Resources` + `ResourcesManager`, and for
`ServiceManager` up to the first actual IPC round trip. `ActivityThread.systemMain()` gets
as far as the graphics stack and stops there.**

Five of six rungs run green (exit 0) on real ART + real `libandroid_runtime.so`, no init,
no zygote, no `system_server`, **no `/dev/binder`**, no emulator.

- Owner / date: approach-D feasibility agent, 2026-11-11.
- Time-box: 45 min; **used ~140 min** (the last 40 were chasing the property service and
  the `Resources` class-init chain, which is where the interesting result is).
- Constraints honoured: no sudo, no mounts outside `bwrap`, no ACOS source crate touched,
  nothing committed, all runs through `tools/guarded.sh`, host RAM peak ~0.4 GB.
- Depends on **E1A** (`results/art-feasibility/E1A.md`): sysroot `.cache/art-exp/sysroot`,
  `research/art-feasibility/a-dalvikvm/scripts/artroot.sh`, its `liblogshim.so`.

Everything is reproducible from `research/art-feasibility/d-framework/` (new files only;
the E1A scripts are untouched):

| path | what it is |
|---|---|
| `native/framework_shim.c` | the one artefact that matters: **`LD_PRELOAD`ed AND `System.load`ed** bionic shim. (1) property service table (`__system_property_get` + `find`/`read_callback`), (2) `dlopen(libandroid_runtime.so, RTLD_GLOBAL)` in the *default* linker namespace, (3) memfd stand-in for `/dev/binder` + the four ioctls `ProcessState::open_driver()` makes, (4) JNI bridge that calls the framework's exported `android::register_*` entry points. |
| `java/dev/acos/e2d/Main.java`, `Nat.java` | the ladder: one subcommand per rung, each printing PASS/FAIL + the first blocker. @hide surfaces by reflection (the SDK stub jar is the compile classpath; the device `framework.jar` is dex-only). |
| `scripts/build.sh` | NDK clang for the shim, `javac` + `d8` for the dex, all into the E1A mini-root (`/data/lib`, `/data/dex/e2d`). |
| `scripts/e2droot.sh` | `artroot.sh` + extra `LD_PRELOAD` (`E2D_PRELOAD`); the only difference. |
| `scripts/run.sh` | `scripts/run.sh all\|bundle\|parcel\|looper\|binder\|resources\|activity`. |
| `.cache/art-exp-d/` | scratch: build dir, per-rung logs (`logs/*.log`). |

```bash
research/art-feasibility/d-framework/scripts/build.sh
research/art-feasibility/d-framework/scripts/run.sh all
```

## 0. Verdict table

| # | step | result | first blocker at this step | fix applied | effort (measured) |
|---|---|---|---|---|---|
| 1 | `android.os.Bundle` (put/get String,int,long,String[]) | **PASS** | none — pure Java over the boot classpath | — | 0 |
| 2a | `Parcel.obtain()` with no runtime library | fails as expected | `UnsatisfiedLinkError: Parcel.nativeCreate` | — | 0 |
| 2b | `System.load("/system/lib64/libandroid_runtime.so")` | **blocked, unavoidable as written** | libnativeloader builds app namespace `clns-1` with `permitted_path=/data:/mnt/expand` → `library "libdl_android.so" (/apex/com.android.runtime/lib64/bionic/…) is not accessible for the namespace "clns-1"` | don't use `System.load`; `dlopen` from an `LD_PRELOAD` ctor in the **default** namespace (what `app_process`/zygote does) | 25 min |
| 2c | `Parcel` writeString/readString + writeInt + `writeBundle`/`readBundle` | **PASS** (84 B, no binder) | `register_android_os_Parcel` is exported but only reachable from the *default* namespace (bionic symbol lookup is namespace-scoped: `dlsym(RTLD_DEFAULT)` in a clns library can't see a RTLD_GLOBAL lib loaded in the default one) | JNI bridge in the **same** `.so` as the loader | 10 min (folded into 2b) |
| 2d | `Parcel.marshall`/`unmarshall` | **PASS** (28 B) | — | — | 0 |
| 3a | `Looper.prepareMainLooper()` | **PASS** | native `MessageQueue::nativeInit` needs `register_android_os_MessageQueue` — and `Looper.loop()` calls `Binder.clearCallingIdentity()` on **every** message, so binder natives are a prerequisite of the message loop | register in `gRegJNI` order: `util_Log` → `os_SystemProperties` → `os_SystemClock` → `os_Parcel` → `os_Binder` → `os_MessageQueue` | 20 min (found the order the hard way, see §2.1) |
| 3b | `Handler.post` to the main looper | **PASS** (queue head inspected; the main looper cannot be quit) | — | — | 0 |
| 3c | private `Looper` thread: prepare + 2 posts + 1 delayed + `quitSafely` + `loop()` | **PASS** (`ran=2`; the not-yet-due delayed message is dropped, i.e. real `quitSafely` semantics) | — | — | 0 |
| 4a | raw probe of `/dev/binder{,h,vnd,binderfs}` | **no device at all** (`stat=-1`; `bwrap --dev` tmpfs) | — | — | 0 |
| 4b | `ProcessState::self()` + `open_driver` + `getContextObject`, driver-less | **PASS** via memfd + 4 ioctls | `open()` needs a mappable fd (`mmap` of `BINDER_VM_SIZE`), `ioctl(BINDER_VERSION)==8`, `BINDER_SET_MAX_THREADS`, `BINDER_ENABLE_ONEWAY_SPAM_DETECTION`, `BINDER_SET_INHERIT_FIFO_PRIO` | memfd (`ftruncate` 1 MB−8 KB) + ioctl interposer; **~90 lines** | 35 min |
| 4c | `ServiceManager.getIServiceManager()` → `ServiceManagerProxy` | **PASS** | `SystemClock.elapsedRealtime()` missing (from `ServiceManager.<clinit>` → `StatLogger`) | registered `os_SystemClock` | 5 min |
| 4d | `ServiceManager.getService("activity")` | **returns `null`** after the driver-less `ioctl(BINDER_WRITE_READ)` is refused — exactly the seam ACOS must fill | `ioctl(fd, 0xc0306201 /*BINDER_WRITE_READ*/) -> ENOTTY` (no servicemanager, no transport) | none (by design); this is the ACOS transport hook | — |
| 5a | `AssetManager` + `addAssetPath(framework-res.apk)`, `addAssetPath(hello-views.apk)`, `list("")` | **PASS** (real `ApkAssets`/`AssetManager2`, cookies 0x1/0x2) | `ApkAssets.nativeLoad` unregistered → register `content_AssetManager`, `content_res_ApkAssets`, `content_StringBlock`, `content_XmlBlock`, `content_res_Configuration`, `os_Trace`. Then `IOException: Failed to load asset path /system/framework/framework-res.apk` — **the E1A sysroot does not contain `framework-res.apk`** | register the 6 entry points; `debugfs dump` of `/system/framework/framework-res.apk` (34 MB) into the sysroot | 15 min + 2 min |
| 5b | `new Resources(am, metrics, config)` + `getIdentifier`/`getString`/`getLayout` | **PASS** — `app_name` = **"Hello Views"** decoded from the fixture APK's `resources.arsc`; `layout/main` resolves and `XmlResourceParser` opens it | `ExceptionInInitializerError` in `android.os.Build$VERSION.<clinit>` → `ArrayIndexOutOfBoundsException: length=0; index=0` (empty `SUPPORTED_ABIS` because there is no property service). `__system_property_get` interposition was **not enough**: API 34 `libbase` reads properties through `__system_property_find` + `__system_property_read_callback` | serve 28 `ro.*` values from a table in *both* bionic paths (**~60 lines**) | 30 min |
| 5c | `Resources.getSystem()` (= `ResourcesManager`, system `ResourcesImpl`) | **PASS** | same `Build$VERSION` blocker | same | (same 30 min) |
| 6a | `ActivityThread.systemMain()` | **blocked at the graphics stack** | `UnsatisfiedLinkError: android.graphics.HardwareRenderer.nSetIsSystemOrPersistent(Z)` — its registration function is **static** in `libandroid_runtime.so` (no `HardwareRenderer` symbol in `.dynsym` at all) | not attempted: needs either a re-link of `libandroid_runtime` with the registration exported, or a hand-written `HardwareRenderer` native | est. **0.5–1 d** for the graphics subset, then `PackageManager`/`Instrumentation` |
| 6b/6d | `currentActivityThread()` / `currentApplication()` | **PASS (null)** — they work and correctly report "no ActivityThread yet" | — | — | 0 |

Wall clock per rung (3 runs, `bwrap` floor 28 ms included): bundle 172 ms, parcel 178 ms,
looper 176 ms, binder 191 ms, resources 215 ms. So the whole framework substrate costs
**~0** on top of E1A's ART process (the shim is 3 preload ctors + 6 dlsym'd calls).

## 1. What the ladder proves, per subsystem

- **`Parcel` is binder-free.** `obtain/writeString/readString/writeInt/writeBundle/readBundle/marshall/unmarshall`
  all run with `libbinder` loaded but `ProcessState` never initialised. E2D therefore closes
  the E1C gap ("we hand-rolled the parcel codec") for the *Java* side: the real
  `android.os.Parcel` and a Rust `acos-parcel` codec can be diffed directly, which is a
  cheap high-value test harness.
- **`Looper`/`MessageQueue` are binder-free too** (epoll + eventfd), *provided*
  `Binder.clearCallingIdentity()` works, i.e. `IPCThreadState` can be constructed. That in
  turn is the only reason the message loop needs the binder stand-in at all: with
  `E2D_BINDER_FAKE=0` the first `loop()` on a *quit-able* looper dies in
  `IPCThreadState`/`ProcessState` (no driver fd).
- **`Resources` needs `Build.VERSION`, and `Build.VERSION` needs a property service.**
  This is the single most under-estimated item in E1A's "no properties: cosmetic" claim:
  `Build$VERSION.<clinit>` indexes `SUPPORTED_ABIS[0]`, which is empty without
  `ro.product.cpu.abilist`, and a failed class init is *sticky* (`NoClassDefFoundError`
  for the rest of the process) — so without properties **no** `Resources`, **no**
  `ActivityThread`, nothing that touches `Build`. 28 table values fixed it.
- **Binder is reachable without a driver, cheaply.** §3.

## 2. Blockers in the order they were hit, with cause/fix/cost

1. **App classloader namespace vs system libraries** (25 min). `System.load` →
   `libnativeloader` → `android_create_namespace("clns-1", ld_library_path=<LD_LIBRARY_PATH>,
   permitted=/data:/mnt/expand)`. Everything in `/system/lib64` and `/apex/*/lib64` is then
   "not accessible for the namespace". First victim `libdl_android.so`
   (`/apex/com.android.runtime/lib64/bionic/`). Setting `LD_LIBRARY_PATH=/system/lib64`
   only moves the failure to the first APEX library. *Fix:* load the library where the
   device loads it — a preload constructor calling `dlopen("libandroid_runtime.so",
   RTLD_NOW|RTLD_GLOBAL)` in the default `[legacy]` namespace. **Zero lines of Android code.**
   *Alternative considered and rejected:* regenerating `/linkerconfig/ld.config.txt` with a
   real `[system]` section — our `linkerconfig` output has only `[legacy]`/`[postinstall]`
   (it needs build.prop/vendor inputs we don't have), and the namespace is created at
   runtime by `libnativeloader` from its own hardcoded permitted paths, so the config file
   is not the lever. Cost if we had needed it: **1–2 d** (would mean shipping a corrected
   `ld.config.txt` + validating against `linkerconfig` output).

2. **Namespace-scoped symbol lookup** (10 min). A `System.load`ed shim in `clns-1` cannot
   `dlsym(RTLD_DEFAULT)` symbols of a `RTLD_GLOBAL` library loaded in the default
   namespace — and `dlopen(..., RTLD_NOLOAD)` by name *and* by absolute path both returned
   `wasn't loaded`. *Fix:* put the JNI bridge in the same `.so` as the loader. Product
   consequence: **one shim, not two** — a detail that would cost a day to rediscover.

3. **Registration order / pending exceptions** (20 min). `register_android_os_Binder` →
   `JavaBinder` class lookups → `StrictMode.<clinit>` → `Log.isLoggable()` (native) →
   `UnsatisfiedLinkError`. The exception is *pending* in the `JNIEnv`, so every subsequent
   `FindClass`/`GetMethodID` in the library returns NULL → 4 `LOG_FATAL`s then `SIGSEGV`
   (exit 139). Note E1A's logshim makes `LOG_FATAL` non-fatal, so you see the four
   `ASSERT … Unable to find class java/lang/Thread` lines *before* the crash instead of an
   abort — a diagnostic trap: they look like the cause, they are the symptom.
   *Fix:* register in `gRegJNI` order (`android_util_Log` first). Cost of getting this
   wrong in a product: it looks like an ART bug for hours.

4. **`libandroid_runtime` exports only a slice of its registration surface** (measured).
   `131` `register_android*` entry points are in `.dynsym` (the rest of `gRegJNI`'s
   several hundred are static — the exact count is not observable from the stripped
   `.so`: `nm` shows no local symbols at all);
   `HardwareRenderer`, `android_view_*` renderers etc. are static. That bounds what a
   no-rebuild integration can reach: everything *plumbing*-shaped is exported
   (`Binder`, `Parcel`, `MessageQueue`, `AssetManager`/`ApkAssets`/`StringBlock`/`XmlBlock`,
   `ActivityThread`, `Process`, `SystemProperties`, `Trace`, `Debug`, `ServiceManager`,
   `SharedMemory`, `MemoryFile`, `Hidl*`, `Vintf*`) — the *graphics/UI* half is not.
   *Fix for the blocked half:* export the statics (binary patch of `.dynsym`/ELF, or
   `patchelf`-style trick) or hand-write the ~5 natives `HardwareRenderer` needs.
   **est. 0.5–1 d** for `systemMain()` to pass; the next walls behind it are
   `PackageManager`/`IPackageManager` (needs a real IPC peer) and `Instrumentation`.

5. **No property service is a hard blocker, not a cosmetic one** (30 min). See §1.
   *Fix:* 28-key table served through **both** bionic read paths (`__system_property_get`
   *and* `__system_property_find`/`__system_property_read_callback`, the latter is what
   API 34 `libbase` uses). **~60 lines.** For a product this wants to be a real
   `/dev/__properties__` area or a `prop_area` in shm so *any* consumer works, and
   `ro.*` should come from the image's `/system/build.prop` (we did not parse it: 3-line
   grep into the table would replace the hardcoded values). Cost: **0.5 d** to do properly.

6. **Sysroot gap**: `framework-res.apk` was not extracted by E1A's `extract-sysroot.sh`
   (only `framework.jar` and friends), so `AssetManager`'s *system* assets fail with
   `IOException: Failed to load asset path /system/framework/framework-res.apk`.
   *Fix:* one `debugfs -R dump`. Recommend adding it (34 MB) to the E1A extraction.

Non-blockers confirmed: no SELinux, no logd (logshim), no tombstoned ("failed to connect
to tombstoned" is cosmetic), no `ro.` properties for `Parcel`/`Looper`/`Binder`/`Resources`
once the table exists, no `/dev/binder`, no zygote, no `system_server`, no SELinux context
lookups (`register_android_os_SELinux` not needed).

## 3. `libbinder` without a binder driver — what it really needs, and the cleanest route

### What `ProcessState::open_driver()` requires (measured, not guessed)

`ProcessState::self()` → `init("/dev/binder", requireDefault=false)` → `open_driver()`:

| requirement | measured on this host | satisfied by |
|---|---|---|
| `open(path, O_RDWR\|O_CLOEXEC)` | `stat(/dev/binder) = -1` (no such file in `bwrap --dev`), `open()` interposed | memfd |
| `ioctl(BINDER_VERSION)` == 0 and out-param == 8 | `0xc0046209` → we write 8 | ioctl shim |
| `ioctl(BINDER_SET_MAX_THREADS)` == 0 | `0x40046205` → 0 | ioctl shim |
| `ioctl(BINDER_ENABLE_ONEWAY_SPAM_DETECTION)` == 0 | `0x40046210` → 0 | ioctl shim |
| `ioctl(BINDER_SET_INHERIT_FIFO_PRIO)` == 0 | `0x40046208` → 0 | ioctl shim |
| `mmap(NULL, BINDER_VM_SIZE, PROT_READ, MAP_PRIVATE\|MAP_NORESERVE, fd, 0)` | works on a memfd of exactly `1 MB − 2*4096` | memfd |

That is **all** of `open_driver()`. With those five answers,
`ProcessState::self()` returns a valid `sp<ProcessState>`, `getContextObject()` builds
`BpBinder(0)`, `ServiceManager.getIServiceManager()` returns a working
`ServiceManagerProxy`, and the *only* thing missing is the transport:
`IPCThreadState::talkWithDriver()` → `ioctl(fd, BINDER_WRITE_READ = 0xc0306201)`,
which we deliberately refuse. `ServiceManager.getService("activity")` then returns `null`
(no `DeadObjectException` storm, no abort) — a clean, well-behaved seam.

### Route comparison (the question asked)

| route | verdict | why |
|---|---|---|
| **bwrap bind-mount of a fake `/dev/binder`** | **not viable** | Nothing bind-mountable gives a mappable, ioctl-answering char device without a kernel driver. `/dev/null`, a regular file, or a unix **socket** file all fail at `open`/`mmap`/`ioctl` (opening a `S_IFSOCK` path gives `ENXIO`; a regular file's `ioctl` gives `ENOTTY`; `mmap(PROT_READ)` of /dev/null fails). A `CUSE`/FUSE char device could fake it, but needs `/dev/fuse` + fusermount privileges — **not available unprivileged**, and it would still be a syscall round trip per ioctl. |
| **`LD_PRELOAD` interposer of `open`/`ioctl` (+ memfd)** | **the clean route — chosen** | ~90 lines, no privileges, no kernel, no mounts. It is also exactly where an ACOS transport belongs: `BINDER_WRITE_READ`/`BINDER_READ_WRITE` is the *single* syscall that carries all binder traffic, so a Rust service can be plugged in behind it with no changes to `libbinder`, `Parcel` or the Java framework. |
| unix-socket *transport* behind the ioctl | **the intended follow-on** | `BINDER_WRITE_READ` gives `struct binder_write_read { size_t write_size; void* write_buffer; size_t read_size; void* read_buffer; }`. The write buffer starts with `BC_*` commands (`BC_TRANSACTION` carries a flat `binder_transaction_data` whose `data.ptr.buffer` **is** a `Parcel`). So: decode `BC_TRANSACTION` → ship the parcel bytes to the ACOS service over the unix socket (E1C already measured 3.8 µs/call) → synthesise `BR_TRANSACTION_COMPLETE` + `BR_REPLY` into the read buffer. Effort: **2–4 d** for a servicemanager + `IPackageManager`-shaped peer, and the ACOS `acos-parcel` codec already reads those bytes. |

Cost of the interposer as built: **35 min**, including finding out that bionic needs
`__open_2` (fortify) interposed as well as `open`.

## 4. What this changes for ACOS

1. **The "framework Java needs a framework" assumption is wrong for the plumbing half.**
   `Bundle`, `Parcel`, `Looper`/`Handler`, `Resources`/`AssetManager`, `ServiceManager`
   and `Binder` object identity all run on ART with no boot. That means ACOS can *reuse
   real framework classes as its app-facing API* rather than reimplementing them or
   app-facing shims, and can validate its own Rust implementations (parcel codec,
   resources table, message queue) byte-for-byte against them.
2. **The wall has moved from "framework" to "IPC peer + graphics".** After E2D the first
   real blocker for an Activity is (a) `servicemanager`/`IPackageManager` (needs the
   transport of §3, not a binder driver) and (b) the graphics registration +
   `HardwareRenderer`/`libhwui`/EGL stack. Neither is a Java/VM problem.
3. **Reusable artefacts** (in increasing order of value): the memfd fake driver (90 lines),
   the `BINDER_WRITE_READ` seam, the property table, and — cheapest of all — the
   default-namespace `dlopen` trick, without which nothing else works.
4. **Estimated cost to productise E2D as "run framework Java classes":** **2–3 d**
   (shim as a maintained library, property table generated from `/system/build.prop`,
   `framework-res.apk` in the sysroot, registration list per subsystem, a `Parcel`
   differential harness vs `acos-parcel`). Getting to a real `Activity` on top of that is
   the §3 transport (2–4 d) plus graphics (est. 5–10 d) — and it is *still* the same work
   ACOS faces with its own VM, because it is Android-subsystem work, not VM work.

## 5. Honest caveats

- Everything is x86_64, Android 14 / API 34, from the same sysroot as E1A. Nothing here is
  architecture-specific except the NDK target triple.
- `E2D_BINDER_FAKE` gates only the memfd *device*; the property table and the
  default-namespace loader are always on. With the fake on, `ProcessState` believes it has
  a driver — code paths that would block on a real driver instead fail fast with `ENOTTY`.
  That is honest but it does mean "binder calls return null/error", not "binder works".
- `[4d] getService("activity")` returning `null` is *success* for a driver-less substrate,
  not a bug: it exercises `BC_TRANSACTION`/`BR_*` handling up to the write.
- `ActivityThread` rung 6 is **partial** by design (the brief said "as far as it gets"); the
  blocker is named, not fixed.
- The `Resources` win depends on a *sticky* class-init ordering: run `Build`-touching code
  before the property shim is installed and the whole process is poisoned
  (`NoClassDefFoundError: android.os.Build$VERSION`). Any integration must install the
  preload before the first Java instruction — which the `LD_PRELOAD` ctor does.
- The `XmlResourceParser` opened successfully but reported `<null>` for the first event's
  name; not investigated (1 API detail, irrelevant to feasibility).
- Numbers are single runs on a shared desktop (other agents' load); the ±20 ms spread
  between rungs is noise, not signal.
