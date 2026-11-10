# `acos-services` — implementation report

**Crate:** `crates/acos-services/`
**Status:** implemented, tested, `clippy -D warnings` clean.
**Scope:** `ServiceHost` + `registry` + `pms` + `atms` + Settings provider +
`display` + the small services. `IWindowManager`/`IWindowSession`/`IWindow`
and the IME interfaces are owned by the concurrent `acos-wm` agent and are
reported as `acos-wm`, never faked.

---

## 1. Design

ACOS runs the real Android 14 framework bytecode. The framework's AIDL proxies
write real `Parcel` bytes through one native (`BinderProxy.transactNative`).
This crate is the other end and has **no VM dependency**: it sees typed values
and `Parcel` buffers only, so every state machine is unit-testable today.

```
native layer
   │  transact(binder_handle, code, &mut data, &mut reply, flags)
   ▼
ServiceHost
   ├─ peek_interface_token(data) ──────────────► descriptor
   ├─ acos_aidl::lookup(descriptor, code) ─────► MethodInfo { name, args, reply }
   │     (mechanically decoded from the real boot DEX — no hand-typed table)
   ├─ data.enforce_interface(descriptor)
   ├─ codec::read_args(data, method.args) ─────► Vec<Value> in decoded order
   ├─ lazy ServiceObject::call(interface, method, args)
   ├─ codec::write_reply(reply, method.reply, values)
   └─ trace (off by default)
```

Three invariants make wrong code fail loudly:

1. **Dispatch is table-driven.** `(descriptor, code)` comes from
   `acos_aidl::lookup`, never from a hand-written match on magic numbers.
   Unknown pairs return `UNSUPPORTED_ANDROID_API { interface, code, method }`.
2. **Arguments and replies are decoded in `WireOp` order.** A handler returns a
   `Vec<Value>`; `codec::write_reply` zips it against the decoded reply ops and
   fails with `BadReply` if the count or a type diverges. An incoming parcel
   that does not match the decoded layout fails with `BadRequest`.
3. **Parcelable layouts come from the framework.** `parcelable::FieldOp` tables
   are transcribed from the real `writeToParcel` bytecode; the scanner in
   `parcelable::scan` (and `examples/scan_parcelables.rs`) is the test oracle.
   `writeToParcel` inheritance is flattened by following `invoke-super`, and
   the API-34 `maybeWriteSquashed` marker is modelled explicitly.

### Module layout

| module | responsibility |
|---|---|
| `host` | `ServiceHost`, `ServiceObject`, `Wiring`, `PendingDelivery`, dispatch, lazy construction, trace, error-reply writer |
| `registry` | `IServiceManager` semantics, `ServiceManager` name→descriptor table, `acos-wm` owner map, standard service install |
| `codec` | `WireOp` ↔ `Value` read/write engine (`build_request`, `parse_reply`) |
| `parcelable` | `FieldOp` codec specs, generic payload read/write, DEX scanner/oracle, curated Bundle/`writeValue` codec |
| `value` | `Value`, `Parcelable`, `Bundle`, `DynamicValue`, `RawFdValue` |
| `exception` | Android `Parcel.EX_*` codes + exception header read/write |
| `trace` | per-call `CallTrace` + `TraceSink`/`MemoryTrace` |
| `pms` | mini-PMS from `acos-apk`, `ApplicationInfo`/`PackageInfo`/`ActivityInfo`, feature + permission policy |
| `atms` | process attach, lifecycle state machine, typed `ClientTransaction` items, `LifecycleEvent` stream, `IActivityClientController` |
| `provider` | Settings `IContentProvider.call`, `GenerationBacking`/`MemfdGeneration` MemoryIntArray boundary, `IContentService` |
| `display` | `IDisplayManager`, `DisplayInfo` |
| `misc` | a11y, input devices, text services, autofill, audio, graphics stats, net, game, virtual device, content capture, voice, AndroidFuture |

### Lazy construction

`ServiceHost::standard()` registers 19 binders (16 service names + the
`IServiceManager`, `IActivityClientController` and `IContentProvider` aux
binders) and parses **no** package data. `check_service(name)` / the first
`transact` constructs exactly that service object; the PMS parses the APK only
inside its factory. `ServiceHost::constructions()` is the observable metric.

### Lifecycle

`atms::AtmsState` is a pure transition table
(`Uncreated → Created → Started → Resumed → Paused → Stopped → Destroyed`,
plus `Relaunch` for config changes). `transition()` returns
`IllegalState` for an illegal move. `schedule()` executes a typed
`ClientTransaction` item by item and emits a typed `LifecycleEvent`; each event
is also converted to `acos_protocol::Event` (`EventBody::Lifecycle`) for the
agent events stream. `activityStopped` emits `Stopped` then
`SaveInstanceState`, matching `lifecycle-summary.txt`. Task basics are
`TaskStack` (push/pop/`clear_above`) and `decide_launch(launchMode, topIsSame)`
(`standard`→new instance, `singleTop`→reuse top, `singleTask`→clear above and
reuse, `singleInstance`→new task).

---

## 2. Public API

```rust
// dispatcher (host.rs)
pub struct ServiceHost { .. }
impl ServiceHost {
    pub fn new() -> Self;
    pub fn standard() -> Self;                       // install the ACOS set
    pub fn install_standard(&mut self);
    pub fn register(&mut self, name: &'static str, descriptor: &'static str,
                    factory: impl Fn(Wiring) -> Box<dyn ServiceObject> + Send + Sync + 'static)
                    -> BinderId;
    pub fn register_aux(&mut self, descriptor: &'static str) -> BinderId;
    pub fn check_service(&mut self, name: &str) -> Result<Option<BinderId>>;
    pub fn descriptor_of(&self, binder: BinderId) -> Option<&'static str>;
    pub fn ensure(&mut self, descriptor: &'static str) -> Result<&mut dyn ServiceObject>;
    pub fn transact(&mut self, binder: BinderId, code: u32,
                    data: &mut Parcel, reply: &mut Parcel, flags: u32) -> Result<()>;
    pub fn invoke(&mut self, binder: BinderId, interface: &str, method: &str,
                  args: &[Value]) -> Result<Vec<Value>>;   // replay helper
    pub fn enable_trace(&mut self, sink: Box<dyn TraceSink>);
    pub fn disable_trace(&mut self) -> Option<Box<dyn TraceSink>>;
    pub fn take_deliveries(&mut self) -> Vec<PendingDelivery>;
    pub fn drain_events(&mut self) -> Vec<acos_protocol::Event>;
    pub fn constructions(&self) -> u64;
    pub fn descriptors(&self) -> Vec<&'static str>;
    pub fn service_names(&self) -> Vec<&'static str>;
}

pub trait ServiceObject: Send {
    fn call(&mut self, interface: &str, method: &acos_aidl::MethodInfo,
            args: &[Value]) -> Result<CallResult>;
    fn drain_events(&mut self) -> Vec<acos_protocol::Event> { Vec::new() }
}

pub struct CallResult { pub reply: Vec<Value>, pub deliveries: Vec<PendingDelivery> }
pub struct PendingDelivery { pub target: BinderId, pub interface: String,
                             pub code: u32, pub method: String, pub data: Parcel }

// codec
pub fn build_request(interface: &str, method: &MethodInfo, args: &[Value]) -> Result<Parcel>;
pub fn parse_reply(interface: &str, method: &MethodInfo, reply: &mut Parcel) -> Result<Vec<Value>>;

// errors
pub enum ServiceError { UnsupportedAndroidApi, NotOwned, UnknownBinder, UnknownService,
                        BadRequest, IllegalState, BadReply, UnsupportedParcelable,
                        Parcel(..), Internal(..) }
impl ServiceError { pub fn code(&self) -> &'static str;
                    pub fn exception_code(&self) -> i32;
                    pub fn evidence(&self) -> serde_json::Value; }

// provider boundary
pub trait GenerationBacking: Send {
    fn generation(&self, ns: Namespace) -> i32;
    fn bump(&mut self, ns: Namespace) -> i32;
    fn file_descriptor(&self) -> Option<ParcelFd>;
}
```

`#![forbid(unsafe_code)]` is on the crate.

---

## 3. Acceptance evidence (real output)

### 3.1 `cargo test -p acos-services`

Full log: `results/services/test-output.txt`.

```text
unittests src/lib.rs        test result: ok. 11 passed; 0 failed
tests/host.rs               test result: ok.  9 passed; 0 failed
tests/lifecycle.rs          test result: ok.  6 passed; 0 failed
tests/parcelable.rs         test result: ok.  3 passed; 0 failed
tests/replay.rs             test result: ok.  2 passed; 0 failed
tests/smoke.rs              test result: ok.  5 passed; 0 failed
                                      ---------
                                      36 passed; 0 failed
```

Covered: replay of every measured outgoing call, display/application round-trips,
Settings generation, lazy construction, unknown-transaction error, token
mismatch, random-byte fuzz (never panics), lifecycle fixture order, lifecycle
property tests, DEX field-order verification, exception-constant verification.

### 3.2 `cargo clippy -p acos-services --all-targets -- -D warnings`

Full log: `results/services/clippy-output.txt`.

```text
    Checking acos-services v0.1.0 (...)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.27s
```
(exit 0, zero warnings.)

### 3.3 Replay coverage

Full table: `results/services/REPLAY_COVERAGE.md` (raw:
`results/services/replay-output.txt`).

```text
replayed 59 rows: 54 ok, 5 not-owned
```

* 54 measured rows owned by this crate return non-error replies.
* 5 rows are deliberately left to `acos-wm` (`IWindowSession`,
  `IInputMethodManager`, `IInputMethodSession`, `IImeTracker`) or the VM
  delivery layer (`IApplicationThread`).
* 1 launch row (`IConnectivityManager.getProxyForNetwork`) is not replayable
  because the interface has no `$Stub` in the Android 14 boot classpath (see
  `results/aidl/subset_misses.txt`); it is listed as a gap.

### 3.4 Startup cost / heap (launch set)

Raw: `results/services/startup-memory.txt` (`/proc/self/status` VmHWM, no
`/usr/bin/time`).

```text
ServiceHost::standard() install: 91924 ns
lazily construct 9 services:     375911 ns
100 replayed checkPermission calls: 1497986 ns (14979 ns/call)
VmRSS:  3000 kB -> 7636 kB
VmSize: 7896 kB -> 9188 kB
VmHWM:  7636 kB
constructions: 9
registered descriptors: 19
registered service names: 19
```

The install path parses no package data; the 376 µs lazy step includes
`acos-apk` parsing `hello-views.apk` for the PMS.

### 3.5 Parcelable codecs

Full table with verification status:
`results/services/PARCELABLE_CODECS.md` (raw:
`results/services/parcelable-output.txt`).

```text
android.graphics.Rect                          Full        4/4    real=4
android.graphics.Point                         Full        2/2    real=2
android.content.pm.PackageItemInfo             Full        9/9    real=9
android.view.DisplayInfo                       Prefix     21/21   real=50
android.content.pm.ApplicationInfo             Prefix     21/31   real=84
android.content.pm.ActivityInfo                Prefix     19/23   real=43
android.content.pm.PackageInfo                 Prefix     10/21   real=43
...
verified 34 specs against the boot DEX (3 full)
```

The `Parcel.EX_*` constants are also verified directly against the Android 14
`android.os.Parcel` static fields in the boot DEX
(`parcel_exception_constants_match_the_framework`).

---

## 4. Gaps (explicit)

1. **API-34 `Parcel` squashing.** `ApplicationInfo.writeToParcel` calls
   `Parcel.maybeWriteSquashed`, and `PackageInfo` uses
   `allowSquashing`/`restoreAllowSquashing`. ACOS models the wrapper as a
   literal "not squashed" int and never squashes. Real-framework
   `createFromParcel` compatibility for these two types therefore needs the
   differential test T-49 (`tools/oracle/parcel-diff`). `DisplayInfo` has no
   squashing and is prefix-verified at 21/21 of the transcribed fields.
2. **`ApplicationInfo` / `PackageInfo` trailing fields.** The leading fields
   (name, packageName, sourceDir/publicSourceDir/dataDir/nativeLibraryDir, uid,
   flags, targetSdk/minSdk, processName, className, theme) are in real order;
   the `SparseArray`/`Parcelling` tail is omitted. `PackageInfo` encodes its
   activity/provider lists as `String[]` (curated) instead of typed arrays.
   `ComponentInfo`/`ActivityInfo`/`ProviderInfo` are prefix-verified.
3. **`bindApplication` parcelable.** The typed `BindApplicationData` is built
   and the lifecycle handshake is complete, but the 24-argument
   `IApplicationThread.bindApplication` parcel (AppBindData with
   `ProviderInfoList`, `ProfilerInfo`, `CompatibilityInfo`,
   `AutofillOptions`, `ContentCaptureOptions`, `SharedMemory`) is not encoded;
   the delivery is skipped rather than written wrong.
4. **`ClientTransaction` bytes.** Typed items
   (`LaunchActivityItem`/`ResumeActivityItem`/`PauseActivityItem`/
   `StopActivityItem`/`DestroyActivityItem`/`RelaunchActivityItem`) are complete
   and drive the state machine; the framework `ClientTransaction.writeToParcel`
   byte layout needs the item parcelables from the VM.
5. **`IConnectivityManager.getProxyForNetwork`.** The interface is absent from
   the boot classpath (the generated AIDL subset records this), so there is no
   mechanically-derived transaction table. ACOS returns
   `UNSUPPORTED_ANDROID_API` rather than guessing a code.
6. **`IPackageManager.notifyDexLoad` argument layout.** The decoded table is
   `Str16, I32, I32, Str16` (the `Map` in the signature is decoded as two
   ints). This is an `acos-aidl` decoder gap for `writeMap`; the replay uses the
   decoded layout, and the handler ignores the arguments (oneway).
7. **IME.** `IInputMethodManager`, `IInputMethodSession`, `IImeTracker`,
   `IRemoteInputConnection` and `IInputMethodClient` are owned by `acos-wm`.
8. **Delivery path.** `PendingDelivery` carries a fully encoded request parcel,
   but the actual `Binder.execTransact` hand-off / looper enqueue belongs to the
   native layer.
9. **Clock.** `atms` uses a logical millisecond counter for event timestamps;
   the VM supplies the real `SystemClock` at integration time.
10. **`IPackageManager.getInstallerPackageName`** returns `null` (side-loaded),
    matching the reference install.

---

## 5. Files

```
crates/acos-services/
  Cargo.toml
  src/{lib,error,exception,value,codec,parcelable,trace,host,registry,pms,atms,provider,display,misc}.rs
  tests/{smoke,host,lifecycle,parcelable,replay}.rs
  examples/{scan_parcelables,measure_host}.rs
results/services/
  REPORT.md  REPLAY_COVERAGE.md  PARCELABLE_CODECS.md  STARTUP_MEMORY.md
  test-output.txt  clippy-output.txt  replay-output.txt  parcelable-output.txt
  startup-memory.txt
```
