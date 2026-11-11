# E1C — Can real ART use **our Rust natives and services**? (approach C)

**Answer: yes, all four tasks pass — with two build-time gotchas and one real
(but cheap-to-fix) design constraint on fork.**

Real Android 14 ART (`dalvikvm64`, API-34 image, bionic, no Android) loads a
**Rust-built `cdylib`**, resolves its JNI natives, calls into it, gets called
**back** from both a Java thread and a native-created thread, hands it
`String`s and `int[]`s, lets it talk over a unix socket to a Rust process that
decodes the bytes with the **real `acos-parcel` codec**, and lets a warm parent
`fork()` children that keep running Java while sharing ~13 MB less per process
than independent spawns.

- Owner / date: approach-C feasibility agent, 2026-11-11.
- Time-box: 40 min; used ~60 min (the two build blockers below cost ~15 min).
- Constraints honoured: no sudo, no `acos` source crate modified (only path
  dependencies, read-only), nothing committed, host RAM peak 0.3 GB (the `N=20`
  fork run), all runs through `tools/guarded.sh`. Scratch in
  `.cache/art-exp-c/` (target dirs deleted at the end).
- Depends on **E1A** (`results/art-feasibility/E1A.md`): sysroot
  `.cache/art-exp/sysroot`, `research/art-feasibility/a-dalvikvm/scripts/artroot.sh`.

Everything is reproducible from `research/art-feasibility/c-jni-services/`:

| path | what it is |
|---|---|
| `native/` | Rust `cdylib` (`libacosjniprobe.so`) + `shim.c` (JNI entry points, NDK clang) |
| `java/dev/acos/artprobe/` | `NativeProbe` (native decls), `Callback`, `Main` (9 checks + benches + fork mode) |
| `service/` | stand-in ACOS `ServiceHost`: unix socket + **real `acos-parcel`** codec (path dep, unmodified) |
| `scripts/build-native.sh` | NDK clang + `cargo --target x86_64-linux-android` → `/data/lib/` of the mini-root |
| `scripts/build-java.sh` | `javac` + `d8` → `/data/dex/artjni/classes.dex` |
| `scripts/run-probe.sh` | starts the service, runs `Main` under `artroot.sh … dalvikvm` |
| `scripts/fork-pss.sh` | N=1/5/20 fork-and-sample-PSS |

Scratch (gitignored, `.cache/art-exp-c/`): `build/` (shim.o + version script),
`lib/libacosjniprobe.so`, `bin/acos-probe-service`, `java/`, `fork/*.log` (raw
PSS runs). Cargo target dirs are deleted after the experiment; the three build
scripts regenerate everything.

```bash
scripts/build-native.sh && scripts/build-java.sh
scripts/run-probe.sh                                  # 9 checks + benches
scripts/run-probe.sh -Xint -Dprobe.mode=bench         # interpreter numbers
scripts/fork-pss.sh 1 5 20                            # sharing
```

## 0. Verdict table

| # | task | verdict | numbers | blockers / effort |
|---|---|---|---|---|
| 1 | Rust `cdylib` with JNI natives, loaded by ART via bionic | **PASS** | 466 KB `.so`, `DT_NEEDED` = bionic `libc.so` + `libdl.so` only; `System.load("/data/lib/…so")` → 8/8 `Java_*` symbols found | 2 non-obvious link flags (§1). **Effort: 0.5 d** to productise into a build step |
| 2 | Java test class: values, String, callback, array | **PASS 9/9** | JNI call **10 ns** (JIT) / **57 ns** (`-Xint`, vs 30 ns interpreted Java call → **27 ns** gate); String round trip **313 ns**; array + callbacks OK | none. **Effort: 0.5 d** for the harness |
| 3 | ServiceHost round trip: native → unix socket → Rust service (`acos-parcel`) → reply | **PASS** | **3.8–4.0 µs**/call from inside ART; **3.64–3.71 µs** for the same client on the host → ART+JNI+bwrap cost **≈0.07 µs**; server-side parcel work **185 ns** ⇒ 95 % is socket syscalls | none functional; a *real* binder transport removes the socket (§3). **Effort: 1 d** to swap the socket for the ACOS service transport |
| 4 | zygote-less warm-parent `fork()`, N=1/5/20 PSS | **PASS (with caveats)** | N=20: **122 MB** total for 21 procs (**5.8 MB/proc**, +0.7 MB marginal); children that dirty 2 MB of heap: **161 MB / 7.7 MB per proc**; independent processes on the same image (E1A): **21.0 MB/proc** | no quiesce/safepoint protocol — works, but is only safe for fork-then-run-on-the-forking-thread (§4). **Effort: 3–5 d** for a production "warm ART" pool |

## 1. Task 1 — Rust `cdylib` that ART can `dlopen` and `dlsym`

Built the straightforward way, because it is the straightforward way:

```
cargo build --release --target x86_64-linux-android      # rust-std for the target installed fine
CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER=<ndk>/bin/x86_64-linux-android34-clang
```

`libacosjniprobe.so` = Rust logic (all `extern "C"`) + a 20-line `shim.c` with
the `Java_dev_acos_artprobe_NativeProbe_*` entry points, compiled by **NDK r27b
clang against real NDK bionic** and linked into the cdylib with
`-Wl,--whole-archive shim.o`. The result has **no** non-bionic `DT_NEEDED`, only
versioned `@LIBC` imports (`malloc`, `connect`, `pthread_*`, `fork`, …), so it is
the same kind of object an NDK app ships.

Two blockers, both invisible until runtime:

1. **`--gc-sections` eats the JNI entry points.** Rust links cdylibs with
   `--gc-sections`; a foreign object's `.text` is unreferenced, so every
   `Java_*` symbol is discarded *silently*. Symptom: `System.load` succeeds,
   every call throws `UnsatisfiedLinkError: No implementation found …`.
   *Fix:* `-C link-arg=-Wl,-u,Java_…` per entry point (generated from `nm`).
2. **Rust's cdylib link localises every symbol that is not part of the Rust
   crate** (`nm` shows `t`, not `T`, in the *full* symbol table, and nothing in
   `.dynsym`). ART's `dlsym` then fails even though the code is linked in.
   *Fix:* `-C link-arg=-Wl,--version-script=exports.map` with
   `{ global: Java_*; probe_*; local: *; };`.

Both are in `scripts/build-native.sh`; together they are the whole "Rust JNI
library" recipe. **This is the reusable artefact of E1C.**

Minor std gap: `std::os::unix::net::UnixStream::set_nodelay` does not exist on
the `*-linux-android` targets (it does on `*-linux-gnu`). Nothing else in std
used here differed.

How it was loaded, and what that proves about the bionic namespace:

```
LOG(3) nativeloader: Configuring clns-1 for other apk . target_sdk_version=0,
       uses_libraries=ALL, library_path=/opt/cuda/lib64:/tmp/.mount_Paseo…,
       permitted_path=/data:/mnt/expand
System.load("/data/lib/libacosjniprobe.so")  → PASS
```

`/data` is in `permitted_path`, so an absolute `System.load` works and a real
app-style `System.loadLibrary` + `java.library.path` is reachable too. Note the
`library_path` is **inherited from the host `LD_LIBRARY_PATH`** — a product
integration must set the classloader namespace explicitly instead of leaking the
host environment into it.

## 2. Task 2 — the JNI surface our service layer needs

`scripts/run-probe.sh` (unmodified ART, no `-Ximage`, default JIT):

```
PASS load-absolute   :: /data/lib/libacosjniprobe.so
PASS add(2,3)==5     :: got 5
PASS echo-string     :: got rust:héllo → ACOS éè 你好        (UTF-8 in + out)
PASS callback-same-thread  :: [ping]                        (CallStaticVoidMethod)
PASS callback-native-thread:: [from-native-thread]          (C thread + AttachCurrentThread + cached global jclass)
PASS int-array-inout :: [3, 5, 7, -7]                       (GetIntArrayRegion / NewIntArray / SetIntArrayRegion)
PASS service-roundtrip :: got 0|pong[0]:HELLO-SERVICE
PASS service-bench                                            (2000 calls, one connection)
RESULT pass=9 fail=0
```

Cost per call (median of a 10⁶-iteration loop, JIT warm; `-Xint` in brackets):

| call | ns/call | note |
|---|---|---|
| Java static call (baseline) | ~0 [30] | the JIT **deleted** the loop at default settings, so the JIT figure is not a baseline |
| `nativeAdd(int,int)` | **10 [57]** | 5.7 M calls/s; `-Xint` gives the honest gate cost: 57 − 30 = **27 ns** of JNI dispatch |
| `nativeEcho(String)` | **313–320 [339]** | includes UTF-8 → Rust `String` → UTF-16 `NewStringUTF` and 2 allocations |
| `nativeCopyIntArray(int[4])` | PASS | one 4-int array; not separately timed |

Comparison with the ACOS Rust VM: **not measured, on purpose.** ACOS's native
dispatch is an in-process Rust `match` on a method id (single-digit ns, no gate
crossing at all), so ART's 10–57 ns is *worse* by construction — the interesting
number is not the delta but that the delta is **irrelevant next to a 3.8 µs
service round trip**. Where ACOS would need a JNI-compatible ABI is
`System.loadLibrary` of *third-party* app `.so`s (§6).

## 3. Task 3 — ServiceHost-style round trip

The Rust service process is `service/` — a 130-line binary that **links
`crates/acos-parcel` by path and is not modified itself**: `crates/` was not
touched, and `acos-parcel` compiles standalone in 1.5 s (its deps are already
vendored/cached).

Wire format is the real one, not a toy:

```
frame: u32 LE length | parcel bytes
request  = int32 policy|0x80000000, int32 workSourceUid(-1), int32 0x53595354 "SYST",
           String16 "dev.acos.artprobe.Probe", int32 seq, String16 payload
server   = Parcel::unmarshall → enforce_interface(descriptor) → read args
reply    = int32 status, String16 text, int64 server-side ns
```

So the native side writes the bytes an AIDL `Proxy` writes, and the Rust side
validates them with the **same code path a real `ServiceHost::transact` uses**
(`enforce_interface` + `read_string16` + `write_string16`); a wrong interface
token is a hard error, not a silent success.

| measured where | ns/call | what it contains |
|---|---|---|
| inside ART, native loop (2000 calls, 1 connection) | **3778 / 4016** (2 runs) | JNI gate + namespace + `connect()`-once + 2×(write+read) + parcel |
| same client on the host, same server (5000 calls) | **3636** | everything except ART/JNI/bwrap |
| server-side dispatch (returned in the reply) | **185** | `unmarshall` + `enforce_interface` + arg read + reply parcel build |

⇒ **the ART/JNI/bwrap overhead on top of a plain host client is ~70 ns (2 %)**,
and **95 % of the 3.8 µs is socket syscalls**, not our codec. Two consequences:

- Reusing `acos-parcel` unchanged as "the wire" to a native service is
  **free**; there is nothing to optimise in the codec for this boundary.
- The remaining 3.6 µs is *transport*. In ACOS the same transact is in-process
  (function call), so a production design should not put a socket in the
  hot path for `BinderProxy.transactNative` — the socket here stands in for
  whatever cross-process boundary a sandboxed app needs, and it costs ~3.6 µs
  (≈275 k transacts/s single-threaded, unbatched).

Note: the fd was reached through a **paths**-based socket
(`/data/service.sock`, served from `.cache/art-exp/data` which `artroot.sh`
bind-mounts as `/data`), not an inherited fd. `bwrap` can pass fds
(`--preserve-fds`), so an inherited-fd handshake is a mount-namespace change, not
a redesign — it is untested here.

## 4. Task 4 — forking a warm parent (zygote-less)

Method: `probe_fork()` calls `fork(2)` from a JNI native; the **parent** records
child pids, the **child returns into Java**, does optional heap work, sleeps,
`Runtime.halt(0)`. PSS sampled from `/proc/<pid>/smaps_rollup`, 4 s after fork.

| N | procs | sum PSS | per proc | marginal vs parent |
|---|---|---|---|---|
| 1 | 2 | 118 MB | 59 MB | — |
| 5 | 6 | 119 MB | 19 MB | ~1 MB |
| 20 | 21 | 122 MB | **5.8 MB** | **+0.7 MB** |

Children that **dirty 2 MB of heap each** (a realistic small-app footprint):

| N | procs | sum PSS | per proc | marginal |
|---|---|---|---|---|
| 1 | 2 | 122 MB | 61 MB | — |
| 5 | 6 | 134 MB | 22 MB | ~2.6 MB |
| 20 | 21 | 161 MB | **7.7 MB** | **+2.6 MB** |

Per-child detail at N=3 with 8 MB of fresh heap each:
`RssAnon` = 16068 kB in the parent, **24260 kB** in every child (+8192 exactly),
while child PSS is only 13.8 MB → anon pages are **shared until written**, and
the 103 MB file-backed boot image stays shared. Compare with E1A's *independent*
processes on the same image: **21.0 MB/proc at N=20** (16 MB marginal). So a
warm-parent fork is worth roughly **15 MB and ~145 ms of init per child**.

What works: fork → return into Java → allocate → sleep → halt, 20 times, no
deadlock, no abort. That is the surprising part and it is the ceiling of the
claim, not a licence:

- The child has **no JIT compiler thread, no GC threads, no SignalCatcher**.
  Allocation of 8 MB worked (bump-pointer, no GC), but *any* child that triggers
  a GC, a deopt, a monitor contention stall or a `pthread` create on the ART side
  is in undefined territory.
- The child inherits the parent's **JNI global refs, fds, and heap locks**. A
  fork that lands while another Java thread is inside a mutator lock (or with a
  safepoint in flight) gives a child that can deadlock on first lock acquisition.
  Real Zygote solves this with `ForkAndSpecialize` bundling + a quiesce/timeout
  protocol; here the parent is single-threaded-in-Java at the fork point, which
  is why it works.
- PSS numbers above are the *optimistic* direction on the other axis too: the
  children immediately overwrite their `pids[]` argument and small stacks, so a
  few dozen pages are already private.

**Verdict: zygote-less forking is feasible as a "warm ART pool" primitive**
(fork N app processes from one warm ART with a quiesce handshake), and infeasible
as "just call fork() anywhere" — the constraint is a protocol, not a flag.

## 5. Blockers and residual risks for a real integration

1. **E1A §5.1 is unchanged and now load-bearing for our path**: every run still
   logs `ABORTER: Check failed: !location.empty() No mainline framework library
   found` (suppressed by the log shim). JNI/natives/Strings/arrays/threads/fork
   are unaffected, but *error handling* inside `libnativeloader` is still the one
   place where ART's own diagnostics are compromised.
2. **Classloader namespace comes partly from the environment** (§1): the
   `clns-1 … library_path=` line showed host paths. A product must set the
   native-library namespace from its own config, not inherit it.
3. **`System.load` vs `loadLibrary`**: only the absolute path was exercised (it
   is what a VM-controlled app sandbox would use). `loadLibrary` + a synthesised
   `java.library.path` is untested but permitted by `permitted_path=/data`.
4. **No GC-pressure or multithreaded-child testing of fork** — the two failure
   modes that a warm-pool design must actually survive (§4).
5. **Service transport is a socket**; the fd was reached by path, and no
   `--preserve-fds` inherited-fd handshake was built.
6. Numbers are single-host medians (n=1 run for the fork PSS table, n≥5 for the
   ns benches except where noted); the host is a shared desktop, min/max spread
   < 5 %.

## 6. What this changes in the ART decision

E1A said real ART buys Java semantics and costs 145 ms + ~16 MB marginal per
process. E1C adds three things:

1. **Our Rust code is not an obstacle.** A Rust `cdylib` behaves exactly like an
   NDK library in ART, including `JNIEnv` callbacks, `AttachCurrentThread` from
   Rust-spawned threads, and primitive arrays. The two build flags are now
   known (§1).
2. **Our service layer is not an obstacle.** `acos-parcel` — unmodified, by path
   dependency — is on the other end of a real transact in **185 ns** of server
   work, and the ART side adds **~70 ns** to a host client. If we ship ART as an
   execution engine, the existing parcel/AIDL decoding carries over as-is.
3. **The process model is cheaper than E1A assumed.** A warm-parent fork puts a
   child at **0.7–2.6 MB marginal PSS** instead of 16 MB, i.e. real ART stops
   looking absurd for many small apps — provided a quiesce/safepoint protocol is
   built (§4), which is 3–5 days, not 30.

Estimated effort to turn this spike into a product path (1 engineer-day ≈ 1
agent-day at spike pace):

| work item | person-days |
|---|---|
| Rust JNI library build integration (version script + `-u` + staging path), template for future natives | 1 |
| Native-service → ACOS parcel bridge: replace the socket with the ACOS transport, keep `enforce_interface` validation | 1–2 |
| Java/native test corpus in CI (the 9 checks as `diff-dexprogs`-style fixtures) | 1 |
| Warm-ART pool: quiesce/safepoint handshake, fork, GC-safety validation, PSS accounting per child | 3–5 |
| **Subtotal for "our natives and services run on ART, with a fork-based app pool"** | **6–9** |

Not included (unchanged from E1A): the Android substrate (logd, tombstoned,
properties) and anything that needs binder/graphics — E1C only proves the
*native and IPC boundary* is not where the "run Android apps on real ART" cost
lives.
