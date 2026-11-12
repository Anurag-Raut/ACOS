# D-M2b — component inventory, CURRENT vs TARGET (ART path)

Diagnosis **D-M2**, half b (architecture inventory). Owner question: *M2 is 107.5 MiB per
session against legacy 78.7; how far is CURRENT from TARGET — one lightweight shared
Android core with many independent sessions — and where would a Zygote-like warm fork and
a shared system-service core actually take us?*

Rules honoured: read-only on `crates/`, `tools/`, `scenarios/`; **no ART runs**; no source
edits; measurements belong to the parallel agent **D-M2a** in
`results/art-runtime/D-M2-MEASURE.md`. Every CURRENT claim carries `file:line`. Anything I
could not read in the code or in a committed measurement is marked **EXPECTED** and has a
falsifiable check listed in §6 for D-M2a. Nothing here is implemented.

Numbers are MiB and, unless marked PSS, are the D2 metric **cgroup `anon+shmem`**, the type
of M2 (`BENCHMARKS.md` §12.2). PSS figures are labelled PSS and never added to a cgroup
figure (`BENCHMARKS.md` §2 rule).

---

## 0. CURRENT process model (the fact the rest of this document is about)

One `acos serve` daemon per host, and **per session**:

```
acos serve  (1 per host; ServeEngine: sessions map, 1.4 MiB alone -- W57 §4.2)
  └─ acos-art launch --apk … --until activity-resumed        <- 1 per session
       |  Rust launcher: ServiceHost (AMS/ATMS/PMS/WMS/IME/…), Logd, C1, input,
       |  probe, watchdog  (crates/acos-cli/src/engine/art.rs:526 spawns it)
       └─ bwrap --unshare-pid/net/ipc/uts/cgroup …            <- 1 per session
            └─ dalvikvm64  (real ART 14, real ActivityThread/ViewRootImpl/HWUI)
                 └─ systemd-run --user --scope (cgroup MemoryMax) wraps the bwrap exec
```

Creation points: `ServeEngine.art: Mutex<HashMap<String, Arc<HeldArtSession>>>`
(`crates/acos-cli/src/engine/serve.rs:83-89`, `:64-73`); the per-session launcher
(`crates/acos-cli/src/engine/art.rs:526-598`); the in-launcher `ServiceHost`
(`crates/acos-art/src/launch.rs:354-381`); the `Logd` (`crates/acos-art/src/launch.rs:322`);
the bwrap command (`crates/acos-art/src/lib.rs:1018-1035`, flags `:899-908`); the guest
`dalvikvm64` (`crates/acos-art/src/launch.rs:644-645`); the mandatory cgroup scope
(`crates/acos-art/src/cgroup.rs:148-200`).

Consequences that drive everything below:

1. **Nothing is shared between sessions except read-only files.** Every session re-execs
   bwrap, re-execs `dalvikvm64`, re-`init`s ART, re-resolves the boot classpath, re-creates
   ~40 `ServiceHost` services, re-reads the APK's resources and re-loads fonts.
2. **The guest is imageless on the product path.** `-Ximage`/`boot.art` exists
   (`crates/acos-art/src/main.rs:247`, `:526-590`; used by one-shot `run` at
   `crates/acos-art/src/launcher.rs:199,214`) but the interactive launch path that
   `acos serve` drives never passes it (`crates/acos-art/src/launch.rs:644-645` has no
   image argument). So the served session is the **cold JIT/interpreter** variant of
   `results/art-runtime/BASELINE.md:17`.
3. **The system services live in a second process, not in the guest.** AOSP's
   `system_server` is one process serving N apps; ACOS has N copies of a Rust
   `system_server`-slice, one per session, each ~**59.4 MiB PSS / 50.6 MiB anon**
   (`results/art-runtime/W12.md:125`). That is the "duplicated system-service state" the
   owner asked about, and it is *not* in the guest heap.

---

## 1. Component table

Legend — **Q1** created once globally / per session / per app process. **Q2** where it lives
and where it is created (`file:line`). **Q3** memory expectation (see D-M2-MEASURE; bands are
my reconciliation of committed numbers, not new measurements). **Q4** code already shared via
Linux file mappings? **Q5** initialised state safely shareable by CoW fork-after-init?
**Q6** could become shared infrastructure with isolated per-session state? **Q7** what must
stay private for correctness/security.

| # | component | Q1 | Q2 process + creation | Q3 expectation (D-M2-MEASURE) | Q4 file-shared | Q5 CoW-forkable | Q6 shared infra possible | Q7 must stay private |
|---|---|---|---|---|---|---|---|---|
| 1 | **ART runtime** (`libart.so`, `libartbase`, `libc++_shared`, `dalvikvm64`) | per app process (N×) | guest; `lib.rs:1018` execs bwrap → `launch.rs:644` execs `/system/apex/com.android.art/bin/dalvikvm64` | PSS `libart.so` 6.0, private dirty 0.1 (`BASELINE.md:92`) → **entirely shared file, ~0 marginal**. Anon ART bookkeeping is inside row 7 | **yes** (ro-bind of the sysroot, `lib.rs:1026-1029`) | **yes** — this is exactly what a zygote child reuses | yes (it *is* the shared code) | nothing: the runtime image is public code. Its *heap* (row 7) must not be shared |
| 2 | **boot image / oat / vdex** (`boot*.art/.oat/.vdex`) | not created on the served path | build input only: `acos-art build-image` → `main.rs:526-590` (`dex2oat64`, 3.7 s, 140 MB, `BASELINE.md:17`) | **0 on the product path today** (imageless). If enabled naively: **+48 MiB private dirty/session** across `boot-*.art` (`BASELINE.md:92`) — a *regression*, see §1.1 | would be (`--ro-bind` of `<data>/img`, `lib.rs:1235-1240`) — but the pages go private-dirty in practice | **yes, and only then is it cheap**: a fork child inherits the *relocated* image space | yes, but only as fork-shared memory, not as a re-exec'd mmap | relocation/code pages are not secret; the *writable* image objects become per-child on first write (CoW), which is correct |
| 3 | **`dex2oat`** (AOT compile) | build/verify only | host-side staging in the bwrap sandbox (`main.rs:568-586`) | ~0 at run time; only during `build-image`/profile compiles | n/a | n/a | yes (already a cache: `build_cache.rs:1-30`) | build artefacts are public; the *cache directory* must not be guest-writable (`S4.md:112`) |
| 4 | **framework classes** (`framework.jar` 38.1 MiB PSS, `core-oj` 5.3, `telephony-common` 3.7, `core-icu4j` 2.8 — `BASELINE.md:92`) | per app process | guest; `BOOTCLASSPATH` from `<sysroot>` via `derive_classpath`, injected at `lib.rs:1192-1226` | DEX text is **file-backed shared clean (private dirty 0.0)**; ~103–113 MiB *fixed* PSS for the whole host, i.e. **~0 marginal per session** (`BASELINE.md:112-114`) | **yes** | no (there is no initialised state to fork — only the mapped jars) | not applicable; the *parsed/resolved* form (rows 5, 12) is the thing to share | the app's own DEX/class loaders (row 15) must stay per session |
| 5 | **preloaded classes / resolved class tables** (`ArtField`/`ArtMethod`/`Class` mirrors, interned strings) | per app process | guest; resolved lazily by ART as the framework runs — no preload step exists in ACOS (contrast `ZygoteInit.preloadClasses`, AOSP §3) | **EXPECTED 10–18 MiB private anon/session**; ART's own bound: cold-path per-session private dirty 15.8–21.0 MiB *total* (`BASELINE.md:37-39`), of which heap/GC/JIT is ~2 MiB each per space | no — this is private anon the first time it is written | **yes, and this is the single biggest AOSP trick**: `Heap::PreZygoteFork` writes it once then freezes it (see §3) | yes: shared read-only pre-fork image, per-child table after fork (`MoveClassTableToPreZygote`) | the child's *post-fork* class table, its own loaders, hidden-API policy state |
| 6 | **JIT / code cache / profile** (`dalvik-cache` per run) | per session (thrown away at exit) | guest + host: per-run cache by default (`lib.rs:1075-1086`, `ACOS_ART_DALVIK_CACHE=shared` opts back into a shared dir) | **EXPECTED 3–6 MiB anon/session** (JIT code + profile bookkeeping; `-Xint` removes it). Not separately measured — D-M2a item | no (per-run dir) | **no**: the child has *no JIT compile thread* after fork (`E1C.md:213`) — a JIT-on child must re-create it explicitly | yes for the *profile*/oat (a shared curated profile is legitimate); no for the JIT code cache | JIT code is derived from app DEX; a shared writable cache is a cross-session poisoning channel (`S4.md` shared-cache row) |
| 7 | **Java heap** (`dalvik-main`, `dalvik-card`, `dalvik-non-moving`, Scudo) | per app process | guest; `-Xms 8m`, growth limit 192m, `-Xmx 512m` (`props.rs:80-82`) turned into runtime flags via `props.rs:45-60` | **EXPECTED 25–40 MiB anon/session** for a served app: `BASELINE.md` measures ~16 MiB *idle* private anon on a bare hello (4 spaces × ~2 MiB + Scudo), W12 measures guest `Pss_Anon` 77.0 with a 24-buffer pool (`W12.md:125`) — the delta is the window/BLAST path | no | **only pre-fork**; a child's heap pages go private on first write. E1C: +0.7 MiB/child with no writes, +2.6 MiB with 2 MiB dirtied (`E1C.md:190-204`) | no — the *pre-fork* part is row 5's frozen zygote space; the post-fork part is per app by definition | **everything**: app objects, statics, monitors, GC roots, JNI global refs |
| 8 | **`ServiceHost` + Rust system services** (AMS, ATMS, PMS, WMS, IME, clipboard, provider, declared, input) | **per session** | launcher process; `acos_services::ServiceHost::new()` + `registry::install_with_handles` at `launch.rs:354-355`, PMS pointed at the APK at `:356`, ATMS seeded at `:362-371`; registry: `crates/acos-services/src/registry.rs:162,168` | **EXPECTED 45–55 MiB anon/session**: the launcher is 59.4 MiB PSS of which 50.6 anon (`W12.md:125`) and the service state is the bulk of it (code ~6 MiB) | partly: launcher binary + `libartshim.so` are file pages (Pss_File 8.8) | **yes in principle** — but only if the fork point is quiescent *and* every service's state is then split | **yes, and this is option B**: one host for N sessions with per-session state keyed by session id/uid | Binder identity/uid, per-app `PackageInfo`/`Resources` for *that* APK, focus/touch/IME target, the session's own task/activity stack |
| 9 | **Binder / `servicemanager`** (ACOS userspace driver + `acos-binder-peer`) | **per session** | launcher process; listener socket + peer thread `launch.rs:324-341,389-420`; the guest reaches it through the `LD_PRELOAD` driver bound at `/data/shim` (`lib.rs:1161-1181`) | **EXPECTED 1–3 MiB anon/session** (socket buffers, `MAX_FRAME` 256 KiB, ≤16 fds/frame — `SANDBOX_CERTIFICATION.md` §1 item 3) | no | **no** — a forked child must get a *fresh* peer socket and fd table (`gOpenFdTable->ReopenOrDetach` in AOSP is the equivalent) | yes: one transport per host, N logical peers keyed by session | the fd table, the socket identity, and **the caller's uid/pid** — this is the trust boundary. A shared transport must never let session A name session B's peer |
| 10 | **C1 compositor** (`surface_client.rs`, `acos-sfgeom`, BLAST decode) | **per session** | launcher process; `crates/acos-services/src/surface_client.rs`, geometry via `use acos_sfgeom::{Display, LayerIn, Snapshot}` (`:70`), composite at `:922-1000` | inside row 8's 50.6 MiB; layers/buffers bookkeeping is small, the *pixels* are row 11 | no | **no**: the layer tree, BufferQueue producers and release callbacks are live per-session state | partly (a shared *geometry kernel* already is shared code); the state cannot be | the layer tree, buffer ownership, frame identity (`frame_seq`), and each session's pixel content |
| 11 | **graphic buffers** (memfd `acos-gralloc`) | per session (2 per launch path, up to 24) | guest allocates `memfd_create("acos-gralloc", …)` (`crates/acos-art/shim/gralloc_shim.c:301`), the fd is dup'd into C1 (`surface_client.rs:276-330`) | **EXPECTED 18.2 MiB shmem/session** at 2 buffers (= 2 × 1080×2209×4, `W12.md:124`); 218 MiB at the 24-buffer workaround. This is cgroup **shmem**, invisible to process PSS (`W12.md:131-138`) | no | no | **partly**: a *buffer pool* pre-faulted once and shared by N sessions is possible only with strict ownership handoff; content sharing would be a leak by construction | **the pixels**: they are the app's output. Cross-session reuse without a full wipe is a data leak |
| 12 | **HWUI / Skia / RenderThread** | per app process | guest; stock `libhwui.so`/`libskia` loaded from the sysroot (`app.rs:336` for the render route, `LaunchMain.java:1607` for graphics natives) | **EXPECTED 5–12 MiB private/session** (Skia caches, `RenderThread`, font/glyph caches). Inside `BASELINE.md`'s 15.8–21.0 cold private dirty, but not isolated there — D-M2a item | the `.so`s yes | **NO**: `RenderThread` is a live thread with a GPU/CPU pipeline. After `fork()` only the calling thread survives (`E1C.md:213`), so a child must restart HWUI/RenderThread; any inherited HWUI mutex can be held | no for the thread; yes for the *font/glyph/paint* caches (a read-only pre-warmed cache is AOSP's `preloadTextResources`) | the render pipeline, draw commands, texture/glyph cache *writes*, and the window's `Surface`/`BufferQueue` |
| 13 | **logging** (`Logd` + `libartshim`/liblog transport) | **per session** | launcher process; `Logd::start(&sockets.0)` (`launch.rs:322-323`), `SOCK_DGRAM` at `/dev/socket/logdw` bound in the sandbox (`lib.rs:1174-1181`) | **EXPECTED 1–2 MiB anon/session** (50 000-entry drop-oldest ring, `SANDBOX_CERTIFICATION.md` §1 item 6) | no | no (a live datagram socket) | yes: one logd ring for N sessions, tagged by session id — this is AOSP's shape | the session's log attribution: a shared log must not let a guest forge or read another session's entries (guest-asserted evidence anyway, §8.6) |
| 14 | **resources** (`framework-res.apk`, `AssetManager`, static RRO/idmap overlays, the app APK) | per app process | guest `AssetManager`s; overlays provisioned host-side at `launch.rs:259-277` (`provision_overlays`), idmaps in `<data>/overlay` bound ro (`lib.rs:1049-1056`) | `resources` rung is **+5 MiB** over hello in the cold path (`BASELINE.md:37-39`: 21.0 vs 15.8 private dirty); `framework-res` file pages are shared clean | **yes** for the APK/`framework-res`/idmaps | **yes**: `Resources.preloadResources()` + a cached system `AssetManager` are exactly what zygote forks (`ZygoteInit.java:137`) | yes: the system `AssetManager` and `framework-res` are per-host; only the *app's* `AssetManager` is per session | the app's own `Resources`/asset path, its package id map, and anything derived from the APK |
| 15 | **fonts** (fonts.xml, `Typeface.sSystemFontMap`, hwui `gDefaultTypeface`) | **per app process** — and ACOS rebuilds it *per session*, which AOSP does once in zygote | guest; `app.rs:33-43` documents that `sSystemFontMap` is a zygote artefact that a bare `dalvikvm64` lacks, so the guest rebuilds it from `/system/fonts` and installs defaults; the font tree is provisioned into the sysroot at `app.rs:630-660` | **EXPECTED 3–8 MiB private/session** (font file mmaps + `Typeface` maps + hwui/Skia glyph caches). Provisioning diagnostics exist for this path (`launch.rs:237` → `app.rs:237`) | font *files* yes; ATLAS/skmia text blobs become private | **yes** — this is the cleanest zygote win in ACOS: one `sSystemFontMapSharedMemory` in the parent, all children CoW it | yes: one host-wide font map, per-session only the glyph-cache writes | nothing secret — but a shared *mutable* font cache is a cross-session channel (glyph poisoning / memory DoS) |
| 16 | **ICU** (`libicuuc`/`libicui18n` + `core-icu4j` + `.dat`) | per app process | guest; code from the sysroot, data mmap'd on first `ICU` use; `core-icu4j.jar` is boot classpath | `core-icu4j.jar` **2.8 MiB PSS / 0.0 private dirty** (`BASELINE.md:92`) → shared; ICU `.dat` mmaps shared clean; **EXPECTED 1–3 MiB private/session** for initialised state | **yes** | yes (`ZygoteInit` warms JCA/ICU-adjacent providers, `ZygoteInit.java:152`) | yes | caches keyed by locale are per-session-safe to share read-only; nothing secret |
| 17 | **caches** — dex cache, class tables, interned strings, hwui caches | per app process | guest; ART-internal (no ACOS code) | see rows 5, 6, 12. Interning is small here (`BASELINE.md` does not isolate it); **EXPECTED 5–12 MiB combined** | no | **yes, and ART has a dedicated mechanism for it** (`InternTable::AddNewTable`, `ClassLinker::MoveClassTableToPreZygote`, `gc/heap.cc:2457-2460`) | yes via the freeze, not via a service | post-fork additions must be private, or session A can observe session B's interned strings |
| 18 | **sandbox infra** (`bwrap` namespaces, guest seccomp guard, Landlock, cgroup, rlimits, watchdog) | **per session** | bwrap exec (`lib.rs:899-908,1018-1035`); guest seccomp `LD_PRELOAD` stub (`lib.rs:1147-1160` -> `crates/acos-art/src/seccomp_guest.c`); cgroup `systemd-run --user --scope` (`cgroup.rs:148-200`); rlimits + `PDEATHSIG` (`launch.rs:656-684`); watchdog (`launch.rs:739-742`) | **EXPECTED 1–2 MiB/session** (bwrap 0.4 MiB PSS for 2 procs, `W12.md:126`; seccomp blob + Landlock rules are tiny). The *cost* is wall time, not memory | the bwrap binary yes; the seccomp BPF blob is a read-only file | **the hard problem** (see §4A): bwrap's setup is an *exec*, and an exec throws the warm image away | yes, but the mechanism must change from "exec bwrap" to "unshare in the forked child" | the namespaces themselves, the uid/gid mapping, the seccomp policy, the cgroup **and the fact that a child may not widen them** |
| 19 | **`acos serve`** (daemon) | once per host | host; `ServeEngine::new` (`serve.rs:98-113`), session map `:83-89` | **1.36 MiB alone** (`W57.md:120`) — already the shared-core shape, and already negligible | yes (binary) | n/a | it *is* the natural home for options B/C | its own session routing table; it must never let one connection address another session's guest |
| 20 | **`acos-art` launcher** | per session | host; spawned at `art.rs:567-598` (`acos-art launch …`, `PDEATHSIG`, own process group) | the *process*; its memory is row 8. The launcher binary itself is `Pss_File 8.8` of `W12.md:125` | yes | **yes**: it holds no per-session state until it installs the services, so the fork point is well defined | yes — option B literally makes this "one launcher, N sessions" | as row 8 |
| 21 | **app data dir** (`/data/app-data/<pkg>`) | per session | host; created at `launch.rs:287` (`run.dir("app-data").join(&package)`), bound rw at `lib.rs:1087-1096` | ~0 resident (files, charged as page cache when touched) | no | no | no | **yes, absolutely**: this is the app's private state and the thing `reset` wipes |
| 22 | **props** (`<data>/acos.properties`) | written **per launch into a shared path** | host; `Props::for_launch()?.write(&root.data.join(PROPS_FILE))` at `launch.rs:317` (comment at `props.rs:415`) | ~0 | it is a shared *writable* file | n/a | it must be per-run, not shared | correctness only: two concurrent launches overwrite one file. Not a secret, but a race — noted as a CURRENT defect, not a memory item |
| 23 | **input state** (focus, touch stream, IME) | per session | launcher (`registry::install_with_handles` → `LiveHandles`, `launch.rs:354-355`) | inside row 8 | no | **no** | yes with strict per-session keying: focus/touch must never cross | **the focus owner and the touch stream**: session A's tap must never land in session B |
| 24 | **display / compositor state** (display list, vsync, `SurfaceControl` ids) | per session (one display per host) | launcher (`acos-services` display + `surface_client`) | inside row 8 | no | no | yes: one display, N layer trees | layer ids and the frame each session may read |

### 1.1 What the table already says about M2

Reconciling the committed numbers gives a **testable decomposition of 107.5 MiB** that
D-M2a can confirm or refute (this is the main thing I want from the measurement):

| slice | EXPECTED MiB/session | source of the band |
|---|---:|---|
| launcher anon (`ServiceHost`, C1, logd, input, capture) | **45–55** | `W12.md:125` (50.6 anon) minus a 24-buffer share |
| guest ART anon (heap, class tables, JIT, ICU/font init) | **30–40** | `BASELINE.md:37-39` (15.8–21.0 bare) + window/BLAST path delta in `W12.md:86` |
| guest shmem (2 gralloc memfds) | **17–19** | `W12.md:124` (18.2 MiB = 2 × 1080×2209×4) |
| bwrap + sockets + cgroup wrapper | **1–3** | `W12.md:126` |
| **sum** | **93–117**, centred on 107.5 | matches M2 |
| *excluded from the sum but real*: shared file pages (sysroot + jars + `libart`) | ~103–113 *host-fixed*, ~0 marginal | `BASELINE.md:112-114` |

If that shape holds, the owner's question has a sharp answer: **M2 is not an ART problem at
all.** Roughly 45–55 % of a session is ACOS's own Rust `system_server`, 15–18 % is
pixel buffers, and only ~30–40 % is the real ART guest — of which the *initialised
framework* part is the only piece a zygote can remove.

Also worth stating plainly: enabling the AOT boot image as the product path would **not**
get us closer to TARGET without a fork. `BASELINE.md:92` measures `boot-*.art` as almost
entirely **private dirty per process (~48 MiB/session)** — a re-exec'd image pays relocation
and image writes per process. A fork child, by contrast, inherits those pages already
resolved. That asymmetry is the whole argument for option A.

---

## 2. The four categories

Exactly as the brief defines them: **(a)** shared files/libraries, **(b)** shared
initialised runtime memory, **(c)** shared services, **(d)** private per-session state.

### 2.1 CURRENT

**(a) shared files/libraries — yes, this is the only sharing that exists today**

* the whole Android sysroot, ro-bound: `/system`, the APEXes, `/linkerconfig`
  (`lib.rs:1026-1047`) — every session maps the same inodes;
* boot classpath jars (`BOOTCLASSPATH`, `lib.rs:1192-1226`), `framework-res.apk`, the static
  overlay idmaps (`launch.rs:259-277`), `/system/fonts` (`app.rs:630-660`), ICU data;
* `libart.so`, `libhwui.so`, `libskia`, `libc`, `libicuuc` (all from the ro sysroot);
* the shims (`libartshim.so`, `libbinder*`, the gralloc shim, the seccomp guest guard),
  ro-bound from `<data>/shim` and `<data>/seccomp` (`lib.rs:1147-1181`);
* the content-addressed build cache for the non-APK artefacts (`build_cache.rs:1-30`);
* `acos`/`acos-art` binaries, the app APK, `framework-res` overlays.

**(b) shared initialised runtime memory — essentially zero**

Nothing initialised survives a session. There is no boot image on this path, no preload, no
pre-warmed font map, no shared `AssetManager`, no zygote space. Every session re-runs
`Runtime::Init`, re-resolves the framework, re-builds `sSystemFontMap` (`app.rs:33-43`),
re-parses the APK in `PmsState::from_apk` (`launch.rs:252`), and re-creates every service.

**(c) shared services — none.** `ServiceHost` is constructed per launch
(`launch.rs:354`), so AMS/ATMS/PMS/WMS/IME/clipboard/provider exist N times. `acos serve`
holds only routing (`serve.rs:83-89`).

**(d) private per-session state — everything else**, including the four things that *must*
stay private: the Java heap and app data (row 7, 21), the binder identity and peer socket
(rows 9, 23), the pixel buffers (row 11), and the trust/sandbox boundary (row 18).

### 2.2 TARGET (one shared core, many independent sessions)

**(a) unchanged and slightly larger**: sysroot + jars + fonts + ICU + the *boot image*
(now actually used, and mapped once by the core). Same mechanism, one more artefact.

**(b) new — and this is the biggest single change**: one initialised ART image per host
(boot image + preloaded framework + frozen class table/intern table + system
`AssetManager` + `sSystemFontMap` + hwui/Skia defaults), shared **read-only** by every
session through CoW after `fork`. Per session: the post-fork delta only (E1C measures
+0.7 MiB with no writes, +2.6 MiB with 2 MiB dirtied, `E1C.md:190-204`).

**(c) new**: one services process for N sessions (AOSP `system_server` shape), with
per-session state keyed by `(session_id, uid)` and a per-session binder peer.

**(d) narrower but still private**: app heap and data dir, the session's own class loaders
and its post-fork class/intern table entries, binder identity + peer socket, focus/touch/IME
target, its own layer tree and pixel buffers, its cgroup/seccomp/uid, and the guest's logs.

---

## 3. How AOSP does it, and where ACOS stands

### 3.1 The zygote preload (`ZygoteInit.java`)

`ZygoteInit.preload()` (`.cache/aosp/base/core/java/com/android/internal/os/ZygoteInit.java:125-152`)
does, in order:

| AOSP step | cite | why it is shared memory |
|---|---|---|
| `ZygoteHooks.onBeginPreload()` / `onEndPreload()` (starts the runtime with *no extra threads*, `SetZygoteNoThreadSection`) | `ZygoteInit.java:165-173`; `art/runtime/native/dalvik_system_ZygoteHooks.cc:446-455` | fork safety: no thread may hold a lock across `fork()` |
| `preloadClasses()` — `/system/etc/preloaded-classes` | `ZygoteInit.java:258-345` | resolves ~thousands of classes **once** |
| `runtime.preloadDexCaches()` | `ZygoteInit.java:347-349` | fills `ArtField`/`ArtMethod` dex-cache slots once |
| `cacheNonBootClasspathClassLoaders()` | `ZygoteInit.java:139-141,380-418` | shared class loaders |
| `Resources.preloadResources()` | `ZygoteInit.java:137` | one system `AssetManager`/`Resources` |
| `nativePreloadAppProcessHALs()`, `maybePreloadGraphicsDriver()` (skippable via `ro.zygote.disable_gl_preload`) | `ZygoteInit.java:94-95,145-150,192-203` | driver load once |
| `preloadSharedLibraries()` — `android`, `jnigraphics`, `compiler_rt` | `ZygoteInit.java:176-189` | so's mapped once |
| `preloadTextResources()` — `Hyphenator.init()`, `TextView.preloadFontCache()` | `ZygoteInit.java:205-208` | **font cache is a zygote artefact** |
| `WebViewFactory.prepareWebViewInZygote()` | `ZygoteInit.java:151` | deliberate skip-able cost |
| `warmUpJcaProviders()` | `ZygoteInit.java:152` | crypto/JCA once |
| `gcAndFinalize()` just before serving forks | `ZygoteInit.java:421-429` | shrink the shared image before it is frozen |

Then the freeze, which is the part that actually makes CoW pay:
`Heap::PreZygoteFork()` (`.cache/aosp/art/runtime/gc/heap.cc:2438-2553`) runs a full GC,
**trims** the non-moving space, then `SetupLinearAllocForPostZygoteFork`,
`InternTable::AddNewTable`, `ClassLinker::MoveClassTableToPreZygote` (`:2457-2460`) and
compacts the surviving objects into a `zygote space` (`CreateZygoteSpace`, `:2544`).
Everything allocated *after* that goes into the child's own spaces. That is why a zygote
child starts with a read-only shared heap instead of a private copy.

### 3.2 The fork + specialize path

* Java side: `Zygote.forkAndSpecialize(...)` → `nativeForkAndSpecialize`
  (`Zygote.java:365-395`), `forkSystemServer` (`Zygote.java:498-513`), `forkUsap`
  (`Zygote.java:665-688`).
* Native side: `com_android_internal_os_Zygote.cpp` `ForkAndSpecializeCommon`:
  mount namespace `ensureInAppMountNamespace` → `unshare(CLONE_NEWNS)` (`:564-572`,
  called at `:855`/`:1938`), per-app data/jit-profile isolation (`:1941-1953`),
  `SetGids` (`:1978`), `setresgid` (`:2008`), **`SetUpSeccompFilter` while still
  CAP_SYS_ADMIN** (`:2014-2017`, table at `:646-662`), `setresuid` (`:2022`),
  `selinux_android_setcontext` (`:2134`), capabilities (`:2126`);
  and the fork itself at `:2468` after `mallopt(M_PURGE_ALL)` (`:2455-2466`) specifically
  "to reduce the number of pages that transition to the private-dirty state".
* The child then re-opens its fds (`gOpenFdTable->ReopenOrDetach`, `:2490`) and calls
  `Runtime::InitNonZygoteOrPostFork` (`dalvik_system_ZygoteHooks.cc:428-445`,
  `runtime.cc:1195`, declared `runtime.h:607`), which is what re-creates the threads the fork did not carry.

### 3.3 What `system_server` shares, and the USAP pool

* `system_server` is **one process per device**, forked from zygote with
  `forkSystemServer`; AMS/ATMS/PMS/WMS are *its* services, not the app's. ACOS has the
  opposite shape: the app's peer *is* the services.
* USAP: zygote keeps a pool of **pre-forked, unspecialised** children
  (`Zygote.java:307-312`, `:654-688`; native table
  `com_android_internal_os_Zygote.cpp:160-171,2283`; see also the `--usap-socket-name`
  note at `ZygoteInit.java:103`), so a launch is "hand a socket to an
  already-forked child" rather than "fork now". Sizes/refill are tunable
  (`ZygoteServer.java:105-127,249-275`). The pool is the latency feature; the *freeze* is
  the memory feature. ACOS needs the freeze first; the pool is optional.
* `app_process` is the small native launcher that starts the zygote runtime; ACOS's
  equivalent is `dalvikvm64` + `LaunchMain` (`launch.rs:644`, `guest/java/dev/acos/art/LaunchMain.java`).

### 3.4 Map onto ACOS: what we already have, what we do not

| zygote step | ACOS today | verdict |
|---|---|---|
| runtime started without threads for the preload window | not applicable: no preload; `dalvikvm64` starts full | **missing** (no `-Xzygote` anywhere; `props.rs:442` only fakes `ro.zygote`) |
| preload classes / fill dex caches | none — resolved lazily per session | **missing** (and AOT image could substitute, but see §1.1) |
| preload resources / system `AssetManager` | built per session inside the guest | **missing** |
| preload fonts (`sSystemFontMap`) | *rebuilt per session from scratch*, explicitly because it is a zygote artefact (`app.rs:33-43`) | **missing — and the code already knows why** |
| preload shared libs / HALs / GL driver | `LD_PRELOAD` per session (`lib.rs:1200-1232`); no driver preload | partial (files shared, no init sharing) |
| freeze heap into a zygote space | none | **missing** |
| fork + specialize (uid, seccomp, mount ns, SELinux) | *emulated by exec*: bwrap does namespaces, the guest seccomp stub does the filter, cgroup does the cap (`lib.rs:899-908`, `cgroup.rs`) | **present but by the wrong mechanism for fork** (§4A) |
| one `system_server` for N apps | N `ServiceHost`s, one per launcher | **inverted** (option B) |
| USAP pool | none. `SessionPool`/`SessionTemplate` in `crates/acos-runtime/src/session_pool.rs` is the **frozen Rust-VM** design (`results/bench/POOL_DESIGN.md`) and is *not* on the ART path | **present only on the frozen path** |
| amortised warm start (145 ms) | each session pays cold ART init; AOT would cut 162–203 ms → 76–87 ms (`BASELINE.md:44`) | **missing** |

Prior art in-tree that a TARGET design must not rediscover: `MEMORY_PROFILE.md:126-147`
(already recommends "a zygote-like template process … fork/CoW where the host permits"),
`POOL_DESIGN.md:94-135` (the honest isolation analysis), `E1C.md:184-231` (fork **measured**:
5.8 MiB/proc at N=20, +0.7 MiB marginal, and the two hard limits: no JIT/GC/SignalCatcher
thread in the child, no quiesce protocol yet), `results/art-runtime/WR-window-surface-options.md:189-196`
(why "boot Android" is not on the table).

---

## 4. TARGET architecture options

Effort is in **one-hour tasks** (`docs/ENGINEERING_POLICY.md`: no task runs >60 min
autonomously). Every option must clear the rewrite gate: ≥25 % of whole-system memory, or
≥50 % of a subsystem with meaningful whole-system savings. All three do; the gate that
matters is compatibility, not size.

### Option A — one per-host ART template, fork per session, re-sandbox the child

**Shared**: rows 1, 2 (now enabled), 4, 5, 6 (profile/oat only), 12–17 as *pre-fork state*,
18's policy (not its per-child namespaces), and ~145 ms of init per child.

**Savings.** With `I` = initialised private anon per session (EXPECTED 30–40 from §1.1) and
`d` = the child's own dirty pages:

```
M2_A  ≈  M2  −  (I − d)  −  (rows 8–11, unchanged unless B is also done)
```

`d` is measured at 0.7 MiB (no writes) / 2.6 MiB (2 MiB dirtied) per child (`E1C.md:190-204`),
and a real app dirties more, so budget `d` = 5–12 MiB. **Band: M2 107.5 → 75–88 MiB/session
(−18 % to −30 %)**, plus ~145 ms off every launch. That alone does **not** clear the 25 %
whole-system gate with B; it does as part of C.

**Isolation risks.**
1. *Fork safety*: after `fork()` the child has one thread; ART's `JIT`, `GC`,
   `SignalCatcher` and HWUI's `RenderThread` are gone (`E1C.md:211-219`). The child must
   re-init the runtime (`Runtime::InitNonZygoteOrPostFork`, `runtime.cc:1195`, declared `runtime.h:607`) and restart
   those threads, or it must run `-Xint`/no-JIT (a compatibility and latency cost).
2. *Inherited locks*: a fork that lands while another thread holds a mutator lock gives a
   child that deadlocks on first allocation. AOSP's answer is the no-thread window
   (`ZygoteHooks.startZygoteNoThreadCreation`) plus a quiesce protocol; ACOS has neither.
3. **The sandbox cannot be re-entered by `exec`.** CURRENT sandboxing *is* an exec of
   `bwrap` (`lib.rs:1018`); a forked child that execs bwrap throws away the warm image, i.e.
   gives up the entire win. So option A requires replacing "exec bwrap" with "unshare in the
   child": `unshare(CLONE_NEWUSER)` works post-fork because the child is single-threaded, then
   `CLONE_NEWNS|NEWPID|NEWNET|NEWIPC|NEWUTS`, then mounts, then the seccomp filter, then
   `setresuid`. Ordering matters and is exactly AOSP's: mount ns *before* dropping
   privileges, **seccomp while CAP_SYS_ADMIN is still held** (`com_android_internal_os_Zygote.cpp:2014-2022`).
   The guest seccomp stub today is installed by `LD_PRELOAD` after exec
   (`lib.rs:1147-1160`, `crates/acos-art/src/seccomp.rs:3-14`), so it moves into
   the child's pre-Java path.
   Alternative if unshare proves unreliable: `clone(CLONE_NEW*)` at fork time, one call.
4. *ASLR shared across children*: every child of one template has identical
   text/library/heap layout, which is a real weakening (AOSP accepts it). Mitigate: the
   template is per-host and short-lived, and `--untrusted` sessions keep the exec path
   (option A only for trusted sessions, explicitly labelled).
5. *Single point of failure*: the template dies → all future sessions die. Existing sessions
   are unaffected (they are children), so this is "fail future launches", not "fail all".

**AOSP reused vs custom.** Reused: ART's zygote space and intern/class-table freeze
(`gc/heap.cc:2438-2553`), `-Xzygote` (`runtime.cc:1616`), `ZygoteHooks` no-thread window,
`InitNonZygoteOrPostFork`, the specialize *order*. Custom: the ACOS launcher becomes the
fork server, the guest `LaunchMain` becomes the post-fork entry, the sandbox becomes
in-process `unshare` (a `bwrap`-less path, or `bwrap --unshare-user-try` replaced by our
own sequence).

**Effort.** 9–14 one-hour tasks: (1) `smaps` proof that the AOT image is relocation-dirty,
(2)–(3) `-Xzygote` + preload harness (host-side, no services), (4) quiesce/no-thread window,
(5)–(6) `fork` + `InitNonZygoteOrPostFork` + one Java allocation + one GC in the child,
(7) in-process sandbox entry (userns/mountns/seccomp order), (8) fd/peer re-open per child,
(9) `LaunchMain` post-fork path, (10)–(11) C1/input/HWUI re-init, (12) `-Xint` fallback mode,
(13) M2 re-measure, (14) hostile-fixture isolation test.

**Smallest experiment that proves it.** 1 hour, no product change: a standalone bwrap-less
parent running `dalvikvm64 -Xzygote`, preloading ~50 framework classes, then
`fork()` → child calls `InitNonZygoteOrPostFork`, allocates 2 MiB, forces a GC, prints a
class identity hash, exits. Measure the child's private dirty with `smaps_rollup`, two
children at once. **Kill-point**: if the child cannot GC without a deadlock in 1 hour of
work, option A is not viable and we stop (per the policy's stop rule).

### Option B — one shared services process for N sessions

**Shared**: rows 8, 9 (partly), 10, 13, 14 (system `AssetManager`), 19, 23, 24 — i.e. the
whole Rust `system_server` slice, keyed per session.

**Savings.**

```
M2_B  ≈  M2  −  (S − s/N)      S = per-session services anon (EXPECTED 45–55)
                               s = the shared process's own overhead, ≈ S + small
```

At N=5 that is `−0.8·S`; with S = 50, **band 107.5 → 65–70 MiB/session at N=1 and
≈ 55–60 at N=5** (the fixed 50 MiB stops being multiplied). For the fixed-core metric M1
this is the *biggest* lever and it directly attacks the owner's "duplicated system-service
state". **Band: −35 to −50 MiB/session, −33 % to −47 % at N=5.** Clears the 25 % gate alone.

**Isolation risks.** Cross-session leakage through every mutable service field (task/activity
stacks, focus, IME target, clipboard, `PmsState`, C1 layers, the binder registry's handle
table); one crash kills N sessions; the binder identity must become per-session
(`(uid, session)` in the transaction header) or an app can address another session's peer;
`--untrusted` needs the shared host to *never* trust a guest-supplied session id.

**AOSP reused**: the shape (one `system_server` for N apps), and its per-app isolation
idioms (uid-keyed state, `ActivityRecord`/`WindowState` keyed by token). Custom: the
keying, because our services are Rust state machines that currently assume one session
(`crates/acos-services/src/host.rs:161-186`, `ams.rs:438`, `atms.rs:400`, `pms.rs`).

**Effort.** 8–12 tasks: (1) inventory every mutable field in `acos-services` and classify
shared/per-session (a mechanical grep-and-list task, 1 h), (2)–(4) key AMS/ATMS/WMS state by
session, (5) PMS: N installed packages in one resolver, (6) per-session binder peer + uid in
the frame header, (7) C1: N layer trees, one display, per-session buffer ownership,
(8) input routing by session, (9) `acos serve` becomes the host owner, (10) crash containment
(a session's panic poisons only its state), (11) M2 at N=1/5, (12) cross-session leak test.

**Smallest experiment.** 1 hour: in a unit test, construct **one** `ServiceHost`, register
two sessions' `PmsState` for two different APKs, and answer `getApplicationInfo` for both
with different packages from the same host. If the current services cannot be keyed without
touching every service, that is the finding — and it bounds the effort honestly.

### Option C — both, i.e. the AOSP shape

`acos serve` = the fixed core (shared services + the ART template) and each session = a
forked guest. This is what the owner described as "one lightweight shared Android core with
many independent app sessions".

```
M2_C  ≈  M2  −  (S − s/N)  −  (I − d)
      ≈  107.5 − 40 (N=5) − 30   ≈  35–45 MiB/session   [band 30–55]
```

i.e. **−55 % to −70 %**, plus 145 ms/launch and a fixed core that is `acos serve` + one
services process + one template (≈ 60–80 MiB for the whole host). This clears the gate with
room, and it is the only option that reaches the legacy 78.7 MiB *and* below it.

Risks are A's ∪ B's, and one more: **two forks in one dependency chain** (services host and
ART template) means a session's identity must be established *before* the guest forks, or
the guest inherits the previous session's peer fd. AOSP avoids this by forking the app from
zygote with a bind-mounted data dir and by having `system_server` separate; ACOS gets the
same by forking *from* the services host process itself (child = guest, parent = services),
which makes the fork point the session-accept point — simple, and it removes the separate
`acos-art launch` process entirely (row 20).

**Effort**: A + B ≈ 17–24 tasks, with B first (it is cheaper per MiB and independently
shippable).

**Smallest experiment**: A's fork probe **plus** B's two-package unit test, then one
combined probe: fork a child from a process that already holds a `ServiceHost`, and have the
child do one binder round-trip through its own peer. That single test exercises both halves
and their interaction.

---

## 5. Recommended path (ordered, with kill-points)

Policy: reuse ART/framework (`.cache/aosp` before guessing), rewrite only for ≥25 % of
whole-system memory, no task >60 min, stop-and-report when an approach fails.

| # | step | why first | kill-point |
|---|---|---|---|
| 0 | **Let D-M2a land.** Confirm the §1.1 decomposition holds (launcher ≈50, guest ≈35, shmem ≈18). | if the launcher is *not* ~50 MiB anon, option B's premise is wrong and only option A is worth doing | the three slices do not sum to 107.5 ± 8 → re-do the inventory against the measurement before any code |
| 1 | **Cheap, non-fork, gate-clearing config wins** (1 h each, no architecture): confirm the 2-buffer launch path is what `serve` uses (not 24); check whether `ACOS_ART_DALVIK_CACHE=shared` is safe for trusted-only and what it saves; check `-Xint` vs default JIT for the served workload (JIT is row 6 + row 12 memory). | 5–10 MiB from a flag is explicitly accepted when it is simple and safe (`ENGINEERING_POLICY.md` rewrite gate) | a flag that changes verified behaviour (frame timing, tap) → reject, do not "fix" it |
| 2 | **Prove the AOT-image private-dirty cause** (1 h, measurement only): two concurrent `-Ximage` sessions, `smaps` of `boot-*.art`; is the 48 MiB/session relocation or runtime writes? | decides whether a fork is *required* for the image win (my EXPECTED answer) or whether a mapping flag is enough | if private dirty is already ~0 with a correct base address, the image is a plain win and the fork is only about the framework heap |
| 3 | **Option B, in slices** — shared services host, per-session keying (8–12 h). | biggest MiB per hour, independently shippable, no fork risk, and it removes one process per session | if keying requires touching >30 mutable fields with no clear session boundary → stop and report; fall back to A for the guest and accept the launcher cost |
| 4 | **Option A fork probe** (1 h, standalone, throwaway): `-Xzygote` + preload + fork + `InitNonZygoteOrPostFork` + one GC in the child. | the only way to know whether fork-after-init is safe here; E1C showed allocation works but GC/deopt is undefined | child deadlocks or aborts on GC within the hour → A is not viable; record and stop |
| 5 | **In-process sandbox entry** (2–3 h): unshare ordering without `bwrap` exec, seccomp after mounts, `setresuid` last. | A is worthless without it; also the highest-risk unknown in the whole path | if the guest cannot pass the existing sandbox certification (`crates/acos-art/tests/sandbox_s5.rs`, `s6`, `cleanup.rs` canaries) with in-process namespaces → keep the exec path for `--untrusted`, use A only for trusted sessions, and say so in the label |
| 6 | **Option C assembly** (4–6 h): fork the guest from the services host at session-accept; delete the per-session `acos-art launch` process. | the owner's stated target; also removes row 20 and the launcher/peer split | M2 at N=5 does not fall below ~60 MiB → the fixed core is not fixed; re-measure before more work |
| 7 | **Re-certify untrusted** and re-measure M1/M2/M5/M6/M7 (2 h). | `--untrusted` must not ride a shared core by accident (`docs/SANDBOX_CERTIFICATION.md` §8.7/8.8) | any untrusted session that can observe shared core state → the opt-in must select the exec path |

**What I would not do**: enable the AOT image as-is on the product path (memory regression,
`BASELINE.md:92`); rebuild a `/dev/binder`; replace HWUI; or share pixel buffers between
sessions. None of them is needed to reach the target, and two of them are actively harmful.

---

## 6. Checks for D-M2a (falsifiable predictions from this inventory)

| # | prediction | how to check | if false |
|---|---|---|---|
| 1 | launcher anon 45–55 MiB/session | per-process `smaps_rollup` on the `acos-art launch` pid at N=5 | recompute levers: B's ceiling is the launcher's real anon |
| 2 | guest ART anon 30–40, shmem 17–19 at the served buffer count | per-process split of the `dalvikvm64` + guest cgroup `memory.stat shmem` | if shmem ≫ 19, the served path is still on the 24-buffer pool and step 1 of §5 is the first fix |
| 3 | `boot-*.art` private dirty ≈48 MiB **per session** if `-Ximage` is used | two concurrent AOT sessions, per-mapping `smaps` | if ~0, the image is already CoW-friendly and the fork's value is only the framework heap |
| 4 | no `acos.properties` collapse under N sessions | N=5 concurrent launches, read the file's content per guest | a real CURRENT race, not a memory item |
| 5 | the served path is imageless (no `-Ximage` in the guest argv) | dump the bwrap argv (`ACOS_ART_DEBUG_ARGV=1`) on a served launch | if an image *is* passed, rows 2 and 6 change and the 30–40 band moves up |

---

## 7. Open questions I could not close read-only

1. Does the served launch mount `/data/fonts`/provision fonts at all, or does the guest
   rebuild `sSystemFontMap` from the sysroot every time? `app.rs:33-43` describes the
   `render-app` route; `launch.rs:237` calls the same provisioning, but I did not trace the
   guest side far enough to claim the font *cost* per session.
2. Is `PmsState::from_apk` (`launch.rs:252`) re-parsed per session, and how much of the
   `ServiceHost` anon is PMS resource tables rather than C1/window state? The 50 MiB
   launcher figure is not decomposed in any committed document.
3. `acos-runtime`'s `SessionTemplate`/`SessionPool` is the frozen path; whether its
   snapshot format could be reused as the *services* half of option B (rather than
   fork) is unexamined and would be a cheap follow-up.
