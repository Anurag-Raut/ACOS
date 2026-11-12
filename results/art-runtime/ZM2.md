# ZM2 — the HOST side of a session: what `acos-art` carries, and what can share it

**Research on Track 3 (long-term session architecture), task ZM2.** Question: today every session
has its own `acos-art` launcher (~17 MiB Pss / ~12 MiB PD per Z3/Z4; measured here at
**23.8 / 18.8** for the same shape). What does that host consist of, and what are the cheapest
architectures that share it safely?

**Production behaviour is unchanged**: every knob added here is env-gated and **unset by default**
(`ACOS_HOST_ZYGOTE_PLAN`, `ACOS_ART_MALLOC_*`, `ACOS_ART_TRIM_AFTER_CAPTURE`). With them unset the
launcher's argv, call order, guest spawn and pixels are exactly what they were; `cargo test -p
acos-cli -p acos-art -p acos-services` passes (§8). Z5 removes the tombstone stub on another
branch, so this document does not touch it.

---

## 0. Verdict

| objective | result |
|---|---|
| Part 1: what the launcher's memory is | **One 16.3 MiB allocator chunk from the capture/PNG path + ~1.4 MiB live host state + ~5 MiB file text/rodata.** The service graph is *not* the weight. With the §4 allocator fix the same launcher is 7.4 MiB at N=1 / 4.0 MiB at N=5. §1 |
| Is the host memory duplicated caches/code? | **No.** 79 % of it is a retained glibc arena/working set created by two `screenshot`/`tree` captures; the pre-built service graph (the thing a shared host would share) is **0.1 MiB resident** (§1.4, §2.4). |
| Prototype C (forked host template) | **Works, passes the isolation matrix at N=1/5/10, 0 px diff.** Marginal launcher **19.15 MiB/session vs 19.66 today = −0.51 MiB (−2.6 %)**; whole-system **+0.3 MiB/session (noise)**. §2 |
| D-lite (one control plane for N sessions) | **Feasible but not worth it**: it would share the same ~1 MiB and must key 8 process-global statics, a global panic hook and an owned `ServiceHost` per peer. Estimate **19.5–20.0 MiB/session today / 3.5–4.0 after W1**, i.e. within noise of C, while one panic/OOM/SIGSEGV kills N. §3 |
| Quick wins independent of architecture | **Capture-buffer fix: a launcher goes 20.48 → 4.01 MiB Pss at N=5 and 23.78 → 7.39 at N=1 (`M_MMAP_THRESHOLD=128 KiB`, measured), i.e. −16.5 MiB/session of marginal host memory, pixels identical, no timing cost.** Then the guest-output tail caps (up to 16 MiB/session, only when a guest is chatty), duplicate APK parses. §4 |

The one-line answer: **the launcher is big because of a per-capture allocator working set, not
because of duplicated session-independent state.** A warm host fork shares the latter (~0.5
MiB/session); nothing in the "one shared control plane" direction can share the former, and the
cheap fix for the former is an allocator/buffer change that is independent of the session model.

---

## 1. Part 1 — measured breakdown of one launcher

### 1.1 Method

`research/zm2/part1.py` imports Z4's driver (`research/zygote-proto/z4/drive.py`), so the sessions,
protocol, taps, isolation checks and md5s are Z4's. It replaces only the sampler: while all N
sessions are held **after the tap**, it dumps every launcher's full `/proc/<pid>/smaps`, its
threads (`/proc/<pid>/task/*/comm`, state, `wchan`) and its fd table (with `fstat` sizes). Runs:

```
tools/art-runtime/run-exclusive.sh --task ZM2 -- tools/guarded.sh --mem 12G --time 900 -- \
  python3 research/zm2/part1.py --mode normal --n 1|5 --out … --json …
tools/art-runtime/run-exclusive.sh --task ZM2z -- tools/guarded.sh --mem 12G --time 600 -- \
  python3 research/zm2/part1.py --mode zygote --n 1|5 --out … --json …
```

All four runs **pass** (count `0→1`, isolation, md5 `39d1e4e3…` initial / `9f3406c1…` after tap).
Raw: `.cache/zm2/{normal,zygote}-{1,5}.json`. "Launcher" = the `acos-art` process; guest, zygote,
bwrap and stubs are excluded (the D-M2 method). Sizes are MiB, Pss / Private_Dirty; N=5 is one
launcher's value (all five are within 0.05 MiB of each other).

### 1.2 Per-component table

| component | normal N=1 | normal N=5 | zygote-mode launcher N=5 | what it is |
|---|---:|---:|---:|---|
| **anon, one ≥8 MiB chunk** | **16.29 / 16.29** | **16.29 / 16.29** | **16.29 / 16.29** | 64 MiB-aligned glibc arena: the frame-capture working set (two `read_buffer_pixels`/`canvas` copies + the PNG `filtered` copy + 1 MiB encode chunks), freed after the capture but **left committed** |
| anon, 1–8 MiB chunks | 1.35 / 1.35 | 1.35 / 1.35 | 1.35 / 1.35 | live per-session host heap (ServiceHost state, APK parse, peer/logd/probe tables) |
| anon, <1 MiB (≈42 mappings) | 0.47 / 0.47 | 1.10 / 1.10 | 1.08 / 1.08 | the rest of the per-session state + thread arenas |
| `acos-art` text (`r-x`) | 4.18 / 0 | 0.84 / 0 | 0.85 / 0 | **file-backed, shared across sessions** (Pss = 4.18/N once others map it) |
| `acos-art` rodata/rw (`r--`/`rw`) | 0.85 / 0.14 | 0.29 / 0.14 | 0.31 / 0.14 | file-backed; the 0.14 is relro/data |
| libc/libstdc++/ld (file) | 0.19 / 0.12 | 0.17 / 0.12 | 0.18 / 0.12 | shared page cache |
| `[heap]` (main arena, brk) | 0.42 / 0.42 | 0.42 / 0.42 | 0.42 / 0.42 | main-thread allocations |
| `[stack]` + vdso | 0.03 / 0.03 | 0.03 / 0.03 | 0.05 / 0.03 | main stack |
| **total** | **23.78 / 18.83** | **20.48 / 19.45** | **20.53 / 19.47** | |
| *with the §4.1 allocator fix* | *7.39 / 2.35* | *4.01 / 2.94 (mean; 5 launchers)* | *(same path, not re-run)* | the ≥8 MiB line disappears; the rest is unchanged |

Thread stacks are *not* a line item because they are tiny: 9 threads, each an 8 MiB VMA of which
8–32 KiB is resident, plus 64 MiB `PROT_NONE` glibc arena reservations (Pss 0). The launcher's
virtual size is ~1.5 GiB; resident is 28 MiB.

**Marginal per session** (`(ΣN=5 − ΣN=1)/4`): normal **19.66 Pss / 19.60 PD**; zygote-mode
per-session launcher **19.69 / 19.63**. The Z4 seam costs **+0.11 MiB at N=1, +0.05 at N=5** (one
extra fd: the guest FIFO; one extra thread: the zygote stdout drain). So the number Z3/Z4 quoted
(~17 MiB Pss) is the *same* host at a lighter capture moment; with the standard driver's two
screenshots it is ~20–24 MiB.

### 1.3 Threads (9 in the normal launcher, 10 in zygote-mode)

| # | name | wchan while held | what it does | files |
|---:|---|---|---|---|
| 1 | `acos-art` | `futex_do_wait` | **main**: `interactive_serve`'s command loop (`recv_timeout` on the stdin channel), cgroup sampling, ready/frame waits | `launch.rs:2759` |
| 2 | `acos-art` | `futex_do_wait` | **peer**: `serve()` — `accept`, `read_frame`, `ServiceHost::transact`, reverse deliveries; blocks on the peer socket/lock | `launch.rs:508`, `:3924` |
| 3 | `acos-art` | `__skb_wait_for_more_packets` | **logd**: `recv` on `logdw` (`SOCK_DGRAM`) | `logd.rs:174` |
| 4 | `acos-art` | `__skb_wait_for_more_packets` | **logd wake/peer recv** second socket waiter (peer connection when idle) | same |
| 5 | `acos-art` | `anon_pipe_read` | **guest stdout** drain → `TailBuffer` (+ `guest-<k>.out` tee) | `launch.rs:987` |
| 6 | `acos-art` | `anon_pipe_read` | **guest stderr** drain | `launch.rs:1011` |
| 7 | `acos-art` | `futex_do_wait` | **watchdog #1**: the process guard installed in `main` (`install_process_guards`) | `watchdog.rs:132`, `:349` |
| 8 | `acos-art` | `futex_do_wait` | **watchdog #2**: `MemoryWatchdog::from_env()` armed on the guest pgid | `watchdog.rs:117`, `launch.rs:1020` |
| 9 | `acos-vsync` | `futex_do_wait` | **frame clock**: the BitTube writer for the session's `IDisplayEventConnection` | `display_events.rs:491` |
| 10 | `acos-art` | `anon_pipe_read` | (zygote mode only) **zygote stdout** drain | `launch.rs:968` |
| — | `acos-art` | (transient) | PNG encoder scoped thread, one per capture (`thread::scope`) | `launch.rs:2189` |
| — | `acos-input-injector` | — | only when `ACOS_INPUT_TAP`/`ACOS_INPUT_KEY` is set; the live driver taps through `InputHost::tap`, so it is absent | `input_dispatch.rs:479` |

Two watchdogs are the only clearly removable thread (quick win W3, 0.02 MiB and one thread).

### 1.4 FDs and buffers (normal N=5, 23 fds)

| fd class | count | owner / charge |
|---|---:|---|
| stdio + launcher stderr file | 3 | driver/session |
| **`/memfd:acos-gralloc (deleted)`, 9 542 880 B each** | **5** | C1's duplicates of the guest's `GraphicBuffer` memfds (2 layer buffers + 3 `BufferCache` entries). **Pixels live in shmem, charged to the guest's cgroup** (D-M2: 18.2 MiB resident = the two touched buffers); the launcher only holds fds, they are not mapped in its smaps |
| `/memfd:acos-settings-generation` 12 B | 1 | settings provider generation counter |
| unix sockets | 5–6 | peer listener/connection, `logdw`, input-channel socketpairs |
| pipes | 5 | stdin, stdout/stderr of the guest, stdout/stderr of the launcher, BitTube pair |
| files | 2–3 | `unsupported.jsonl`, `guest-<k>.out/err` tee |
| zygote mode adds | +1 | `guest.fifo` (and `session.json`/`argv` writes) |

No mmap'd frame buffers: `capture_latest` reads through the launcher's own dup with `pread`, builds
a `Vec` (that is the 16 MiB working set), and drops it. Nothing file-backed is privately
relocated: the binary text/rodata are the only launcher file maps and they are shared.

### 1.5 Per-session by necessity vs needless duplication

| item | MiB/session | verdict |
|---|---:|---|
| **capture working set retained by glibc** (16.29) | **16.29** | **needless** — two `screenshot`/`tree` calls leave three frame-sized buffers committed after they are freed; §4 W1 |
| live host heap, 1–8 MiB chunks (1.35) + most of the <1 MiB anon (≈0.8) | **≈2.2** | **necessary** — mutable per-session service state (PMS/ATMS/WMS/display/compositor/input), peer tables, probe tree, logd ring. Not shareable without keying (D-lite) |
| thread stacks (9 × 8–32 KiB) | ~0.1 | necessary; one watchdog removable |
| guest stdout/stderr `TailBuffer` (cap 8 MiB each, grows only as the guest writes) | 0.0–16 | **necessary bytes, oversized cap**: 0.8 MiB written at N=5 here; a chatty app can retain 16 MiB/session |
| binary text/rodata + libc (file) | 5.0 RSS, **0.2 unique** | already shared by the page cache; a private copy would be the only way to make it worse |
| gralloc memfds | 0 in Pss | necessary per-session pixels (shmem), already deduped by ownership |
| `[heap]`/main stack/arena reservations | 0.5 | necessary |

**Nothing session-independent is large.** The pre-built service graph + pre-warmed keychar maps —
the only thing a host zygote can share — is **0.1 MiB of resident heap** (§2.4).

---

## 2. Part 2C — forked host template (implemented, OFF by default)

### 2.1 Design

`ACOS_HOST_ZYGOTE_PLAN=<plan.json>` makes one `acos-art` process the **warm host template**:

1. it does the session-independent prefix of `launch()` once (data root, linkerconfig, classpath,
   guest dex, render/art/binder shims — `crates/acos-art/src/launch.rs` `launch()`),
2. it pre-warms `acos_services::keychars::stock()` and builds **one empty `ServiceHost`** with the
   full standard service set installed (`host_zygote::make_host` → `ServiceHost::new` +
   `registry::install_with_handles`), **no APK installed**, no thread,
3. it `fork()`s one child per plan slot. Each child `dup2`s its own fds 0/1/2 (the plan names
   inherited descriptors), sets its per-slot env, installs **its own APK into its private CoW copy
   of the host**, and runs `launch_session()` — its own run root, sockets, logd, peer thread, guest.
   The template only `wait4`s.

That shares binary text/rodata, libc, the pre-init heap and the empty service graph as **CoW**, while
every child remains a separate process with its own failure domain, pidns/cgroup identity and
runtime. Files: `crates/acos-art/src/host_zygote.rs` (new, 330 lines), the `launch()`/`launch_session()`
split and the `ACOS_HOST_ZYGOTE_PLAN` branch in `launch.rs` (~40 changed lines), and
`research/zm2/drive_hostfork.py` (a driver that hands each forked child its own per-session
socketpair; z4's session matrix reused unchanged).

### 2.2 Verification (driver: `research/zm2/drive_hostfork.py`)

| N | verdict | tree 0 → tap → 1 | isolation (every other session stays 0) | md5 initial / after tap | frames |
|---:|---|---|---|---|---|
| 1 | **pass** | 1/1 | — | `39d1e4e3…` / `9f3406c1…` | 7 / 59 |
| 5 | **pass** | 5/5 | 10/10 ordered reads `0` | identical to the normal path | 7 / 59 |
| 10 | **pass** | 10/10 | 45/45 ordered reads `0` | identical | 7 / 59 |

`failures=[]` in all three JSONs (`.cache/zm2/hostfork-{1,5,10}.json`). Ready line: max **199 ms**
at N=1, **233 ms** at N=5 (normal N=5: 233.8 ms), **318 ms** at N=10. Fork is invisible in the
protocol; the template never writes a ready line (its stdio is redirected to `template.log`).

### 2.3 Memory

| N | normal: ΣPss (launchers) | C: template + children ΣPss | normal per launcher | C per child | C marginal vs normal |
|---:|---:|---:|---:|---:|---:|
| 1 | 23.78 | 24.38 (2.51 template + 21.87 child) | 23.78 | 21.87 | −1.9 (the template is a new fixed cost) |
| 5 | 102.42 | **100.98** (1.04 + 5 × 19.99) | 20.48 | 19.99 | −1.44 total |
| 10 | *(not run)* | **207.47** (0.88 + 10 × 20.66) | — | 20.66 | — |

**Marginal (N=1→5 slope): C 19.15 MiB Pss/session and 19.17 MiB PD/session vs today's 19.66 /
19.60 = a 0.51 MiB/session Pss (2.6 %) and 0.43 MiB PD (2.2 %) saving.** At N=10 the per-child
value is 20.66 (the N=10 driver run had 10 concurrent guests and more log/tree traffic; the N=5→10
slope is 21.3 MiB/session), so C shows **no further saving with N** — the shared part is a fixed
~2.5 MiB, not a per-session amount.

Whole system (driver tree, all roles): normal 238.78 (N=1) / 610.11 (N=5);
hostfork 235.84 / 633.00 / 1074.20 (N=10). Marginal 92.8 vs 93.2 MiB/session — **C is inside the
guest's run-to-run variance** (§6). The launcher-only comparison is the clean one.

### 2.4 Why the saving is small (the point of the exercise)

Measured on the same runs (one process's smaps groups, MiB Pss/PD):

| | template | forked child | normal launcher |
|---|---:|---:|---:|
| `[heap]` | 0.09 / 0.09 | 0.42 / 0.42 | 0.42 / 0.42 |
| anon ≥8 MiB (capture working set) | **0.00** | **16.28 / 16.28** | **16.29 / 16.29** |
| anon 1–8 MiB + <1 MiB | 0.05 | 2.38 / 2.37 | 2.45 / 2.45 |
| text + rodata (file) | 0.81 / 0.01 | 0.85 / 0.01 | 1.13 / 0.14 |
| **total** | **1.03 / 0.10** | **19.99 / 19.11** | **20.48 / 19.45** |

The template's whole shareable content is **1.03 MiB of Pss, 0.10 MiB of it heap**. The children
re-dirty almost all of the pre-built host graph (they install their own APK and run their own
window), so the only pages that stay shared are the text/rodata and the untouched part of the
empty service graph. That is the 0.5 MiB/session. Fork-sharing a host whose state is per-session by
construction cannot do better; **there is no "duplicated cache" left to remove.**

---

## 3. Part 2D-lite — one control-plane process for N sessions (feasibility only, no build)

Two shapes are possible; both keep `ServiceHost` per session:

* **D-lite(a)**: N `ServiceHost`s + N `Peer`s in one process (each session's peer thread as today).
* **D-lite(b)**: one `ServiceHost` behind a lock shared by N peers. Rejected: `Peer` **owns** its
  host (`crates/acos-binder-peer/src/peer.rs:138`, `with_host` at `:209`) and `ServiceHost` is
  `&mut self` per transaction (`host.rs:593`); (b) would serialize every binder transaction of every
  session behind one mutex, and the first panic poisons it. This document estimates (a).

### 3.1 Process-global state that would leak between sessions (file:line)

| item | file:line | leak if shared as-is |
|---|---|---|
| `static IME: Mutex<Option<Arc<Mutex<ImeState>>>>` (`ime::shared`) | `crates/acos-services/src/ime.rs:249` | one IME (and its editor/focus state) for all sessions |
| `static CLIPBOARD: Mutex<…>` (`clipboard::shared`) | `clipboard.rs:248` | session A can paste session B's clipboard |
| `static OVERRIDE: OnceLock<Mutex<Option<DisplayOverride>>>` | `display.rs:91` | one display override (size/density/orientation) per process |
| `thread_local! LAST_DISPLAY_INFO` | `display.rs:649-651` | the last display reply of whichever session used *this* thread |
| `static STOCK: OnceLock<StockMaps>` | `keychars.rs:594` | read-only after init — safe to share (this is what C pre-warms) |
| `thread_local! LAST_DESCRIPTION / LAST_ACTIVITY / LAST_PROVIDER / LAST_PROVIDER_LIST` | `pms.rs:38-42` | PMS memoisation attributed to the wrong session |
| `thread_local! SETTINGS_PROVIDER_BINDER` (+ peers) | `provider.rs:27-33` | settings binder routing |
| `static BOOT_DEX: OnceLock<Vec<DexFile>>` | `parcelable.rs:2922` | read-only (tests only) — safe |
| `static LAST_STAGE: Mutex<Option<String>>` | `crates/acos-art/src/launch.rs:1558` | panic/stage evidence names another session's stage |
| panic hook (global) | `launch.rs:1606` (`install_session_panic_hook`) | a panic writes `session-exit.json` + a `HOST_PANIC` protocol line into the **last installed session's** out dir |
| env mutation `ACOS_KEYCHARS_DIR` | `launch.rs:262` (set per launch) | process-wide; benign today (same value), a future per-session value would not be |
| process cwd / umask / rlimits / `PDEATHSIG` / signal mask | `main.rs` guards, `launch.rs` `pre_exec` | one process, one set |
| the per-session `props` file and `<data>/app/<apk>` | `launch.rs:333` (APK copy), `:437` (props), `app.rs:928` (overlays) | shared writable paths; already racy today across launchers (identical content) |

Per-session objects that *need no change* (each is already `Arc<Mutex<…>>` inside a `ServiceHost`):
WMS, ATMS, PMS, display, compositor/SurfaceState, declared services, input, logd, peer.

**Threads that would be shared** (one process): the 9 threads of §1.3 × N, reducible to ~3N+3 (one
protocol main loop, one logd ring, one stdin reader, one shared vsync writer, N peers, 2N pipe
readers). Thread stacks are ~0.1 MiB/session, so this saves ~0.05–0.1 MiB/session of *kernel* memory,
not the 16 MiB.

### 3.2 Failure domain

* A **panic in a peer thread** unwinds only that thread (`panic=unwind` is the default; the
  workspace sets none), so one session's protocol dies while the others survive — but the global
  panic hook attributes the record to the *last* session that installed it (wrong evidence), and a
  panic while holding a process-global mutex poisons it for N (`unwrap_or_else(into_inner)` hides it).
* A **panic in the multiplexing main thread**, an allocation failure (`abort`), a SIGSEGV/UB in any
  service instance, or `std::process::abort` kills **all N** sessions and their guests.
* **OOM**: one process = one cgroup = one `memory.max`; one session's growth kills the group (the
  same defect Z4 §5(d) measured for the ART zygote, now unavoidable).
* **DoS**: no per-session CPU; a busy peer thread competes with every other session's peer.
* **Evidence**: logd, `unsupported.jsonl`, `session-exit.json` and `LAST_STAGE` need per-session
  keys before N sessions can be told apart.

### 3.3 Estimate (labelled)

From the measured rows, the shared part is **1.03 MiB** and the per-session live part is ~19.0 MiB
before W1, ~2.7 MiB after it:

```
D-lite(a), today        ≈ C's child (19.99) − per-process kernel overhead (page tables, task/vma,
                          9 thread kernel stacks) ≈ 19.5–20.0 MiB Pss/session at N=5
D-lite(a), with W1      ≈ 4.0 (measured launcher) − the same kernel overhead ≈ 3.5–4.0 MiB/session
```

i.e. **within ±0.5 MiB/session of prototype C**. The only place D-lite could beat C is the *free*
part of the 16.3 MiB capture working set: one allocator heap would let session k+1 reuse the chunks
session k freed, whereas C's per-process heaps each keep their own. That is a real advantage while
captures do not overlap — but it is exactly the slack W1 removes (after the fix there is no large
free chunk to amortise), and it disappears as soon as two sessions capture concurrently. The
failure-domain and evidence-attribution costs (§3.2) are permanent; the memory saving is not
there. **Not recommended.**

---

## 4. Quick wins independent of architecture

| # | win | MiB/session | risk |
|---|---|---:|---|
| **W1** | **Do not retain the capture working set — measured: −16.5 MiB/session.** Two screenshots leave one 64 MiB-aligned glibc arena with a freed 16.3 MiB chunk committed (`read_buffer_pixels` + composite `canvas` + PNG `filtered`, `surface_client.rs:1201,2389,2559`, `launch.rs:3692` + the scoped encoder thread at `launch.rs:2179`). Measured knob `ACOS_ART_MALLOC_MMAP_THRESHOLD_KB=128` (+ `ACOS_ART_TRIM_AFTER_CAPTURE`) in §4.1. A production fix streams the stored-block zlib directly from `rgba` (no `filtered` copy), reuses one canvas per launcher, or allocates the three buffers with `mmap` so a free returns them. | **−16.5 (79 % of the launcher; 20.48 → 4.01 MiB at N=5), pixels and ready time unchanged** | low for the allocator knob (one `mallopt`), medium for the encoder/canvas rewrite (must keep the PNG byte-identical; the stored-block emitter is deterministic) |
| W2 | **Cap the guest-output tails for interactive sessions.** `DEFAULT_STREAM_CAP` = 8 MiB **per stream** (`capture.rs:31`) × stdout+stderr = up to 16 MiB/launcher; here the guest wrote 0.8 MiB so the win is 0. For a verbose app it is 16. | 0 here; up to 16 | medium: the tail is crash evidence; make it 1–2 MiB or tee-only-to-file |
| W3 | **One watchdog, not two.** `install_process_guards` (main) + `MemoryWatchdog::from_env` (launch) start two sleeping threads per launcher. | 0.02 + 1 thread | low; keep the per-guest one, arm the host ceiling from it |
| W4 | **Parse the APK once.** `PmsState::from_apk` runs for `application_class`/`theme` (`launch.rs:370`), again in `install_package_from_apk` (`host.rs:578`), plus `manifest_labels`, `apk_launcher_activity`, `launch_budget` and `native_libs::install` — 4–5 manifest/APK passes per session, each with transient allocations. Reuse one `PmsState`. | 0.5–2 (transient/peak; app-dependent) | low; behavior-preserving refactor |
| W5 | **Gralloc pool:** 5 dup'd 9.54 MB memfds per launcher (2 layer + 3 cache). Only the touched buffers cost shmem (D-M2: 18.2 MiB). Lower `ACOS_MAX_ACQUIRED_BUFFERS` only if the framework tolerates fewer; the cache entries are free unless the guest cycles many buffers. | 0 (fd count only) | low for the cache, medium for the pool |

### 4.1 W1 measured end to end (same driver, normal path)

**Baseline (this tree's HEAD):** the `≥8 MiB` anonymous line is **16.29 MiB Pss/PD per launcher**
at N=1 and N=5, normal and zygote seam alike — 79 % of a 20.5 MiB launcher. It appears only after
the first `screenshot`/`tree`; the host template, which never captures, has 0.00 MiB in that band.

**Attribution:** each screenshot allocates three frame-sized buffers (`read_buffer_pixels` ≈ 9.93
MiB, composite `canvas` ≈ 9.93 MiB, PNG `filtered` ≈ 9.93 MiB) plus a 1 MiB encode-chunk ring that
the **PNG encoder thread** allocates and the **main thread** frees (`launch.rs:2179`). The freed
large chunks land in a glibc **worker-thread arena** (the 64 MiB-aligned mapping) where glibc's
top-trim does not run, so they stay committed between captures.

**Measured with real ART sessions** (`research/zm2/sweep.sh`, same driver and checks; all runs
`verdict=pass`, both md5s identical to the normal path):

| run | launcher ΣPss / ΣPD (MiB) | mean Pss / PD | ready max | md5 |
|---|---:|---:|---:|---|
| normal N=1 (HEAD) | 23.78 / 18.83 | 23.78 / 18.83 | 1334 ms* | `39d1e4e3…` / `9f3406c1…` |
| normal N=5 (HEAD) | 102.42 / 97.23 | 20.48 / 19.45 | 234 ms | identical |
| `ACOS_ART_MALLOC_MMAP_THRESHOLD_KB=128`, N=1 | **7.39 / 2.35** | 7.39 / 2.35 | 182 ms | identical |
| `…MMAP_THRESHOLD_KB=128` + `TRIM_AFTER_CAPTURE=1`, N=1 | **7.30 / 2.32** | 7.30 / 2.32 | 189 ms | identical |
| `…MMAP_THRESHOLD_KB=128` + trim, N=5 | **20.06 / 14.69** | **4.01 / 2.94** | 217 ms | identical |
| `ACOS_ART_MALLOC_ARENA_MAX=1` + trim, N=1 | 32.71 / 27.74 | 32.71 / 27.74 | 192 ms | identical |

\* the N=1 baseline's ready time includes first-run effects; N=5 is the comparable row.

**Marginal per session: 19.66 → 3.19 MiB Pss and 19.60 → 3.09 MiB PD** (`(Σ5 − Σ1)/4`) — a
**−16.5 MiB/session** host cost, with the frame bytes, frame numbers and ready latency unchanged
(217 vs 234 ms at N=5; the capture is cheaper because the buffers are `mmap`/`munmap`'d rather than
retained). `M_ARENA_MAX=1` is **worse** (32.7 MiB): one main arena keeps the freed chunks in the
brk heap where `malloc_trim` cannot reach all of them. `M_TRIM_THRESHOLD` alone /
`malloc_trim` alone do not fix it; the **`M_MMAP_THRESHOLD=128 KiB` is the one knob that does**, in
the real launcher and in the allocator-only reproduction (`research/zm2/capture-alloc.c`, which
reproduces 15.5 MiB retention and 0.28 MiB with the knob, no ART needed).

**Production fix (recommended, no env knob):** remove the `filtered` copy (stream the stored-block
zlib straight from `rgba`), reuse one canvas per launcher, and allocate the frame buffers with
`mmap` so a free returns them (or set `M_MMAP_THRESHOLD` once in `launch()` before any thread).
The PNG bytes stay identical — the stored-block emitter is deterministic.

Whole-system effect at N=5: 92.8 → ~76 MiB/session (−18 %), which is larger than the entire Z4
zygote saving as implemented (15.3 %) and comes with no security-model change.

---

## 5. What this means for Track 3

1. **Do not build a shared host to save host memory.** The host is ~2.2 MiB of live per-session state
   plus one 16.3 MiB allocator artefact; the only session-independent part is 1.0 MiB. Prototype C
   measures the ceiling of safe sharing: **0.5 MiB/session**, and a shared control plane cannot beat
   it by more than kernel overhead. After the W1 fix the whole host is ~3.2 MiB/session, so the
   shareable fraction is even smaller in absolute terms.
2. **Do fix the capture path.** Measured at **−16.5 MiB/session** (launcher 20.48 → 4.01 MiB at
   N=5; 79 % of the host) — more than the entire Z4 zygote saving (29.4 % of the guest-tree
   marginal) at a fraction of the risk, and it removes the host from the "large share of each
   session" problem.
3. The **ART zygote (Z4) and the host-template (C) are complementary but C is not worth shipping**:
   Z4's shared 40 MiB pre-fork core is real shared state; the host's is not.

---

## 6. Limitations / not measured

* Zygote-mode launchers were measured at N=1/5 only (the driver's Z4 build); the per-session
  launcher is the same code path and differs by +0.1 MiB (fd + drain thread).
* Normal-path N=10 was not run (C was). Guest memory varies ±5 MiB/session between runs (the N=5
  hostfork guests totalled +26 MiB vs the normal runs), which is why whole-system deltas are noise
  and the launcher table is the honest comparison.
* The 16.3 MiB chunk's allocation site is attributed by size, alignment, the encoder thread, the
  allocator-only reproduction (`capture-alloc.c`) and the measured `M_MMAP_THRESHOLD` fix — not by a
  heap profiler (none installed; smaps + code reading per the brief).
* `M_MMAP_THRESHOLD` was measured with and without `TRIM_AFTER_CAPTURE`; the trim adds nothing on
  top of the threshold (7.30 vs 7.39 at N=1 is run noise), so the knob alone is sufficient.
* D-lite was not built; its estimate is labelled and derived from the measured rows.
* `cargo test -p acos-cli -p acos-art -p acos-services` with all knobs unset: see §8.

---

## 7. Reproducing

```bash
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
CARGO_TARGET_DIR=/home/Qui/Projects/acos/.cache/zm2-target cargo build --release -p acos-art
# prototype C build (contains the env-gated seams)
CARGO_TARGET_DIR=/home/Qui/Projects/acos/.cache/zm2c-target cargo build --release -p acos-art

# Part 1 (normal / zygote per-session launchers)
tools/art-runtime/run-exclusive.sh --task ZM2 -- tools/guarded.sh --mem 12G --time 900 -- \
  python3 research/zm2/part1.py --mode normal --n 5 --out .cache/zm2/normal-5 --json … 
tools/art-runtime/run-exclusive.sh --task ZM2z -- tools/guarded.sh --mem 12G --time 600 -- \
  python3 research/zm2/part1.py --mode zygote --n 5 --out .cache/zm2/zygote-5 --json …

# prototype C
ACOS_ART_BIN=$PWD/.cache/zm2c-target/release/acos-art \
tools/art-runtime/run-exclusive.sh --task ZM2c -- tools/guarded.sh --mem 12G --time 840 -- \
  bash -c 'for n in 1 5 10; do python3 research/zm2/drive_hostfork.py --n $n \
    --out .cache/zm2/hostfork-$n --json .cache/zm2/hostfork-$n.json; done'
```

## 8. Tests

All knobs unset (the production path), whole workspace crates touched by this task:

```
CARGO_TARGET_DIR=/home/Qui/Projects/acos/.cache/zm2c-target CARGO_BUILD_JOBS=4 \
tools/art-runtime/run-exclusive.sh --task ZM2tst -- tools/guarded.sh --mem 12G --time 900 -- \
  cargo test -p acos-cli -p acos-art -p acos-services
```

**rc=0**, **64 × `test result: ok`**, **0 × `FAILED`**, 0 panics (log:
`.cache/zm2/tests.log`). The real-session checks in §2.2 independently exercise the
`launch()`/`launch_session()` split (the forked children run `launch_session` and produce the exact
normal-path md5s/frames), and §4.1's four knob runs all report `verdict=pass` with the same md5s as
HEAD, including the runs where the knobs *are* set.
