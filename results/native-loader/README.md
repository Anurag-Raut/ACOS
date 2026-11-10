# ACOS native loader — results

Crate: `crates/acos-native-loader` (package `acos-native-loader`).
Scope of this document: acceptance evidence for the Rust-owned ELF/Bionic
native loader, design notes, the current compatibility ledger, and open risks.

All numbers below were produced on the ACOS host (glibc 2.44, Rust 1.90,
NDK 27.1 / clang, LLVM 19) with the commands shown. Artifacts in this
directory: `coverage.json`, `load_times.json`.

---

## 1. Public API

```rust
use acos_native_loader::{Loader, LoaderConfig, LibraryHandle, LogSink, MissingPolicy};
use std::os::raw::c_void;
use std::path::Path;
use std::sync::Arc;

// configuration
let config = LoaderConfig::default()
    .with_search_path("/path/to/apk/lib/x86_64") // DT_NEEDED search dir
    .with_missing_policy(MissingPolicy::Strict)   // or LazyTrap
    .with_log_sink(Arc<dyn LogSink>);             // captures __android_log_*

let loader = Loader::new(config);

// load from a file or from bytes (e.g. extracted from the APK)
let lib: LibraryHandle = loader.load_file(Path::new("libhellojni.so"))?;
let lib: LibraryHandle = loader.load_bytes(&bytes, "libhellojni.so")?;

// symbol lookup (JNI_OnLoad, Java_*)
let jni_on_load: *const c_void = lib.symbol("JNI_OnLoad").unwrap();

// honest reporting + teardown
let report: Report = loader.report();   // serde JSON
loader.unload(&lib)?;
```

`LoaderError::code()` returns stable strings: `ELF_MALFORMED`, `WRONG_ABI`,
`NATIVE_DEP_NOT_FOUND`, `NATIVE_INIT_FAILED`, `UNSUPPORTED_NATIVE_SYMBOL`,
`UNSUPPORTED_RELOCATION`, `NATIVE_IO_ERROR`, `NATIVE_SYMBOL_NOT_FOUND`,
`NATIVE_INVALID_ARGUMENT`.

---

## 2. Design notes

### 2.1 ELF loading, mapping, relocation

* Dependency-free, bounds-checked, panic-free ELF64 parser (`src/elf.rs`,
  `src/dyninfo.rs`). It parses the header, program headers, `PT_DYNAMIC`,
  `DT_GNU_HASH` and `DT_HASH`, `.gnu.version`/`.gnu.version_r` (parsed;
  versioning is otherwise ignored for lookup).
* An anonymous `PROT_NONE` reservation covers all `PT_LOAD` vaddrs; each
  segment is then mapped `PROT_READ|PROT_WRITE`, its file image copied in, and
  after relocation the final protections are applied (`R`, `R|X`, `RW`).
  W^X is respected: pages are never simultaneously writable and executable.
  `PT_GNU_RELRO` is held writable just long enough for `IRELATIVE`, then sealed
  read-only.
* Relocations applied: `RELATIVE`, `GLOB_DAT`, `JUMP_SLOT`, `64`, `PC32`,
  `32`, `32S`, `COPY`, `DTPMOD64`, `DTPOFF64`, `IRELATIVE`. `NONE` skipped.
* Relocation container formats: `DT_RELA`, `DT_REL`, `DT_JMPREL` (REL/RELA),
  standard `DT_RELR` (RELR word encoding), `DT_ANDROID_RELR`, and
  `DT_ANDROID_RELA` APS2 packed. REL/RELR addends are read in place.
  The APS2 decoder handles the single `*_RELATIVE` group the Android packer
  emits; multi-group sections are rejected with an error instead of being
  mis-decoded.
* `DT_NEEDED` is resolved recursively. System libraries (`libc.so`, `libm.so`,
  `libdl.so`, `liblog.so`, `libandroid.so`, `libz.so`, `libEGL.so`, …) are
  satisfied by the built-in shim table rather than by files; a real file for a
  dependency (e.g. `libc++_shared.so`) in the search path is loaded as a guest
  object.
* Symbol binding is done in a final pass after the whole object graph is
  mapped, so forward references and ELF interposition work. Search order is
  load order (the referencing/earlier object wins), then the bionic table.

### 2.2 TLS strategy

The NDK's default on Android is **emulated TLS**: `__thread` variables become a
`__emutls_control` object and per-thread storage is obtained through the
*weak, statically-linked* `__emutls_get_address`, which rides on
`pthread_key_create/getspecific/setspecific/once/mutex_*`. We provide all of
those shims, so emulated TLS works for guest threads created via our
`pthread_create` (verified: `tc_run_all` bit for `tc_threads` == 0, including a
guest thread that writes its own `__thread` variable).

We additionally implement real ELF TLS for libraries built with the general
dynamic model: each object with `PT_TLS` gets a monotonic module id and an
aligned per-thread block (initialised from the TLS image), keyed off a host
`pthread_key_t`; `__tls_get_addr` returns `block + offset`. `DTPMOD64`/`DTPOFF64`
are supported. `initial-exec`/`local-exec` relocations (`R_X86_64_TLS_TPOFF`,
`R_X86_64_TLSDESC`) require fixed offsets from the real thread pointer and are
rejected with `UNSUPPORTED_RELOCATION` rather than mis-linked.

Host `%fs:0x28` is the same stack canary as bionic's on x86_64, so
`-fstack-protector` guest code needs no translation; `__stack_chk_fail` is
forwarded to glibc.

### 2.3 pthread strategy

Bionic's `pthread_mutex_t` (40 B), `pthread_cond_t` (48 B), `pthread_attr_t`
(56 B), `pthread_rwlock_t`, and the `*attr_t` types are **not**
layout-compatible with glibc's, so guest storage is treated as a handle: the
host object is keyed off the guest object's address. A zeroed guest mutex/cond
(the bionic static initializers) is created lazily as a default host object.
`pthread_create` runs a Rust trampoline that establishes the guest TLS state
before invoking the guest start routine, and `pthread_join` maps back to the
host `pthread_t`. Key/once/setname/getname APIs have identical ABI and are
forwarded.

### 2.4 Unwinding strategy

The C++ test library is built with static libc++ and static libunwind
(`-static-libstdc++`), so `_Unwind_*`/`__cxa_*` live inside the guest object
and are self-contained. The unwinder finds FDEs through `dl_iterate_phdr`,
which this loader implements: it reports every mapped guest object with its
real program header array (including `PT_GNU_EH_FRAME`) and load bias
(`dlpi_addr = base − min_vaddr`). `Phdr` is `#[repr(C)]` so the guest reads it
as `Elf64_Phdr`. `libc++_shared.so` is *not* required by the test; whether to
load it as a guest dependency is left open (its import coverage is tracked
below).

### 2.5 Varargs strategy

Stable Rust cannot define C-variadic functions, so the variadic entry points
live in a small C support object (`csrc/guest_shims.c`, built with the host
`cc` crate — justified use of the host compiler, no Android runtime code):

* `__android_log_print/vprint/assert` format into a buffer and call a Rust
  sink (`acos_log_write`) so the runtime can capture logcat-style lines, plus
  `syslog`/`openlog`/`closelog` and variadic stdio (`fprintf`) trampolines when
  a bionic `__sF` stream pointer must be translated.
* JNI `Call*Method(...)` trampolines `va_start` and read the arguments using
  the method descriptor supplied by the test harness
  (`acos_tj_arg_types`), then call `acos_tj_call`. The array forms
  (`Call*MethodA`) are implemented directly in Rust.

### 2.6 The bionic compatibility table

Every import is classified and auditable:

* **Forwarded** (383 symbols): the bionic and glibc ABI is identical, so the
  symbol resolves to the host libc/libm. Examples: `malloc`, `memcpy`,
  `strtol`, math, `open`, `mmap`, wide-character, locale, dirent, `__*_chk`
  fortify wrappers, `__cxa_atexit`, `__cxa_finalize`, `__register_atfork`.
  The `stat` family is forwarded because the x86_64 bionic `struct stat` is
  byte-for-byte identical to glibc's (verified with clang record-layout dumps).
* **Shimmed** (92 symbols): translating implementations in this crate —
  `__errno`, `__sF` + stdio translation, `sigaction`/sigset, pthread handle
  indirection, `__system_property_get` (returns Android 34 defaults),
  `__android_log_*`, `android_set_abort_message`,
  `__android_get_application_target_sdk_version`, `__tls_get_addr`, `__assert2`,
  `__strlen_chk`/`__strrchr_chk`, and the `dl*` family.

Anything absent is **never** silently stubbed:

* `MissingPolicy::Strict` (default) fails the load with
  `UNSUPPORTED_NATIVE_SYMBOL`, listing every missing symbol and the library
  that needs it.
* `MissingPolicy::LazyTrap` binds each missing symbol to a distinct generated
  x86_64 stub that names the exact symbol and aborts when (and only when)
  called.

The test-only JNI harness (`src/testjni/`) implements the full 233-slot
`JNINativeInterface` (4 reserved + 229 functions) in exact spec order; every
unimplemented slot points at its own aborting trap.

---

## 3. Tests — `cargo test -p acos-native-loader`

```
Running unittests src/lib.rs
  elf::tests::rejects_bad_magic
  elf::tests::rejects_short_input
Running tests/cpp_features.rs
  cpp_features_and_destructor_on_unload
Running tests/deps.rs
  dependency_order_and_interposition
Running tests/fuzz.rs
  parser_rejects_empty_and_tiny_inputs
  parser_survives_seeded_mutations_and_truncations
Running tests/jni.rs
  hello_jni_end_to_end
Running tests/leak.rs
  load_unload_1000_times_does_not_leak_mappings
Running tests/native_features.rs
  c_library_all_features_including_packed_relocations
  log_lines_are_captured_by_the_sink
Running tests/policy.rs
  arm64_library_is_reported_as_wrong_abi
  lazy_trap_binds_and_reports_symbol
  missing_dependency_is_structured
  strict_mode_reports_all_missing_symbols
Doc-tests acos_native_loader
  crates/acos-native-loader/src/lib.rs - (line 9)
```

Result: **15 passed, 0 failed** (14 integration/unit tests + 1 doctest).

What the tests cover:

| Area | Test |
|---|---|
| C lib: malloc/memcpy/strlen/snprintf/errno/log, pthread mutex+create/join, `__thread`, constructor, static pointer reloc, qsort, getenv/clock_gettime/time | `native_features` |
| Plain RELA, Android APS2 packed RELA, RELR | `native_features` |
| Log capture through a Rust sink | `log_lines_are_captured_by_the_sink` |
| C++: `std::string`, `std::vector`, `new`/`delete`, exceptions caught in-library, static destructor on unload | `cpp_features` |
| `DT_NEEDED` ordering + interposition | `deps` |
| Strict missing-symbol error, LazyTrap, `WRONG_ABI`, missing dependency | `policy` |
| 1000 load/unload with `/proc/self/maps` line-count check | `leak` |
| Parser fuzz: 20 000 seeded mutations/truncations | `fuzz` |
| JNI end-to-end (`libhellojni.so`) | `jni` |

### 3.1 `libhellojni.so` end-to-end output

`cargo run -p acos-native-loader --example jni_smoke`:

```
JNI_OnLoad -> 0x00010006
registered = [RegisteredNative { class: "dev/acos/fixtures/hellojni/MainActivity",
              name: "registeredEcho", signature: "(Ljava/lang/String;)Ljava/lang/String;", fn_ptr: 140101819546784 }]
stringFromJNI = "Hello from native C"
add(2,3) = 5
sumArray = 15
calls = [CallRecord { name: "javaCallback", is_static: true, args: [Int(21)] },
         CallRecord { name: "voidCallback", is_static: false, args: [Ptr(94570558531536)] }]
pending exception = Some(Exception { class: "java/lang/IllegalStateException", message: "thrown from native" })
registeredEcho = "echo!"
```

### 3.2 Missing-symbol report

`cargo run -p acos-native-loader --example missing_report`:

```
code = UNSUPPORTED_NATIVE_SYMBOL
unsupported native symbols: [LibraryMissing { library: "libtestmissing.so",
                              symbols: ["acos_definitely_missing_symbol"] }]

LazyTrap loaded; trap stubs allocated = 1
report = { "bionic_table": { "forwarded": 383, "shimmed": 92, "total": 475 }, ... }
```

### 3.3 Lint

```
$ cargo clippy -p acos-native-loader --all-targets -- -D warnings
    Finished ... (0 warnings)
```

### 3.4 unsafe inventory

46 `unsafe { ... }` blocks across 7 modules, each with a `SAFETY`/`# Safety`
comment; the rest of the ABI surface is `unsafe extern "C" fn` shims.

| module | unsafe blocks |
|---|---|
| `src/loader.rs` | 22 |
| `src/tls.rs` | 7 |
| `src/testjni/mod.rs` | 6 |
| `src/bionic/pthread.rs` | 6 |
| `src/bionic/trap.rs` | 3 |
| `src/bionic/dl.rs` | 1 |
| `src/testjni/jni_impl.rs` | 1 |
| **total** | **46** |

---

## 4. Measurements

### 4.1 Load time — `cargo run --release ... --example bench`

200 load+unload iterations, release build, p50/p95. The host `dlopen` row is a
**rough reference only** — glibc's linker does much more, and `libhostref.so`
is a tiny glibc library (4 relative relocations), so this is *not*
like-for-like.

| library | engine | relocs | p50 | p95 | mean |
|---|---|---:|---:|---:|---:|
| libhellojni.so | acos-native-loader | 9 | 8.5 µs | 11.0 µs | 9.2 µs |
| libtestcpp.so (static libc++) | acos-native-loader | 1801 | 287 µs | 311 µs | 294 µs |
| libhostref.so | host `dlopen` (reference, not like-for-like) | 4 | 7.8 µs | 9.9 µs | 8.4 µs |

(`load_times.json`.) The small fixture is within ~10 % of host `dlopen`; the
large C++ object is dominated by per-relocation symbol resolution (linear
global scope scan) and is the obvious optimisation target.

### 4.2 Bionic table

| category | count |
|---|---:|
| forwarded to host glibc/libm | 383 |
| translating shims | 92 |
| **total** | **475** |

### 4.3 Import coverage of real third-party x86_64 Android libraries

Parsed only — these libraries were **not** mapped or executed
(`coverage.json`):

| library | imports | forwarded | shimmed | missing |
|---|---:|---:|---:|---:|
| `libc++_shared.so` (NDK 27.1) | 157 | 117 | 40 | **0** |
| `libRSSupport.so` (build-tools 35) | 164 | 123 | 41 | **0** |
| `librsjni_androidx.so` | 15 | 7 | 5 | 3 |
| `librsjni.so` | 15 | 7 | 5 | 3 |
| `libc.so` (renderscript intermediate) | 5 | 0 | 5 | **0** |
| `libm.so` (renderscript intermediate) | 8 | 8 | 0 | **0** |

The only remaining missing imports are `AndroidBitmap_getInfo`,
`AndroidBitmap_lockPixels`, `AndroidBitmap_unlockPixels` (the
`libjnigraphics` bitmap API, not implemented). That is the current
coverage work-list.

---

## 5. Unsupported ELF features / missing bionic areas

ELF / relocation:

* `R_X86_64_TLS_TPOFF`, `R_X86_64_TLSDESC` (initial-exec/local-exec TLS) — the
  general-dynamic model is supported, but these require real thread-pointer
  offsets.
* Multi-group `DT_ANDROID_RELA` APS2 sections (only the single `RELATIVE` group
  emitted by the Android packer is decoded).
* `R_X86_64_GOTPCREL`/`GOTOFF`/`PLT32` etc. are absent from normal shared
  objects; if encountered they fail with `UNSUPPORTED_RELOCATION`.

Bionic areas not implemented (imports will be reported missing):

* `libjnigraphics` (`AndroidBitmap_*`).
* `getaddrinfo`/`freeaddrinfo` and the name-service structs.
* `libutils`/`libcutils`/`libbase` C++ helpers beyond what is forwarded.
* Binder, `libEGL`/`libGLESv2`/`libvulkan` real entry points (table entries
  exist for import satisfaction but there is no graphics implementation).
* `__system_property_find`/`read_callback` (return NULL/no-op), `dladdr`
  reports the object but not the nearest symbol name, `dlsym(RTLD_NEXT)`
  unsupported.
* `pthread_*` beyond the implemented set (barriers, spinlocks, cancellation,
  robust mutexes, `pthread_atfork` beyond forwarding).

---

## 6. Open risks (real-world libraries)

* **`libc++_shared.so`**: import coverage is now 0 missing, and it loads
  through the same path as any guest object, but it has not been *executed*
  here (the C++ test uses static libc++). Its ELF TLS / exception runtime
  interactions and `__cxa_*`/`operator new` interposition need a dedicated
  end-to-end run before claiming support.
* **`libflutter.so` / `libreactnative*.so`**: large, use much more of libc
  (locale, `dlopen` of assets, `pthread` variants, `getaddrinfo`, `mmap`
  exec, `__register_atfork`), plus V8/Flutter JIT pages requiring `PROT_EXEC`
  mappings created by guest `mmap` (currently forwarded to host `mmap`, which
  is fine on the host). The main risks are missing libc surface (tracked by
  coverage) and ELF TLS models.
* **`initial-exec` TLS** in libraries linked for performance would fail to
  load; this is a hard limitation of the pthread-key TLS design.
* **`stat`/`dirent`/locale structs**: forwarded only after layout verification;
  other 32-bit ABI differences are out of scope (x86_64 is the only target).
* **Performance**: symbol resolution is a linear scope scan per relocation. It
  is fine for the fixtures and the analysed libraries, but a large app with
  thousands of imports across many `.so`s will need a global hash index.
* **Thread safety of shims**: `dl*` and the bionic table assume a
  process-global current loader; concurrent independent loaders in one process
  are not yet supported.
* **`ptrace`/unwind across host frames**: only guest objects are reported to
  `dl_iterate_phdr`; unwinding from a guest frame through a host frame is not
  supported.
