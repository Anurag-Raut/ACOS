# D-M6 — where M6 reset→ready spends its 0.70 s

Time box: 60 min, started 2026-11-12T12:37Z. **Diagnosis only**: the two
env-gated trace sinks below are inert when their variable is unset (no reply,
error or control-flow change). Nothing is implemented.

## 1. Result

One `acos_reset` on a held `acos serve` ART session costs **702.6 ms p50**
(n=18, 3 reps × 7 resets, first reset of each rep discarded) on this host. The
W57 value is 681.8 ms p50; this run is 3 % slower and noisier (host load1
4.1–6.6 vs W57's ~2), so the *split* is the deliverable, not the absolute.

**Three phases are 78 % of the reset:**

| rank | phase | p50 | share | file:line |
|---|---|---:|---:|---|
| 1 | **frame bind** — the `tree` command the reset runs after the relaunch | **238.0 ms** | 34 % | `crates/acos-cli/src/engine/art.rs:1899` → `crates/acos-art/src/launch.rs:2167` |
| 2 | **post-resume fixed sleep** — `Thread.sleep(200)` before `activity-resumed` | **201.7 ms** | 29 % | `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java:1276` |
| 3 | **guest stop** — `close` → child reaped → group proven empty | **105.0 ms** | 15 % | `crates/acos-cli/src/engine/art.rs:1490` (`sleep(50)` at `:1507`) |

Within (1): the `tree` command first runs `wait_for_settled_observation`, whose
quiet window is a hard-coded **150 ms** (`launch.rs:1878`), then
`capture_c1`, whose PNG encode alone is **60.3 ms** (`encode_png_rgba`,
`launch.rs:1703`). Measured capture total 82.0 ms p50; settle + tree JSON =
**153.6 ms p50**. Within (3): `shutdown()` writes `{"cmd":"close"}`, then polls
`child.try_wait()` every **50 ms**, so the stop quantises to 55 / 105 / 155 ms.

The 640 ms budget is reachable **without touching isolation**, by either (2) or
(1) alone; both together are ~440 ms of removable work.

## 2. Phase table (n=18, one reset per row, p50/p95)

Host: load1 5.06 → 4.10 → 6.58 across the 3 reps, MemAvailable 42 GiB; every
rep ran under `run-exclusive.sh --task DM6` +
`guarded.sh --mem 8G --time 900`. Build: HEAD + the two trace hunks, release,
`CARGO_TARGET_DIR=.cache/dm6-target` (`binaries.sha256` in the evidence dir).
APK `fixtures/build/apps/counter.apk`; each reset is preceded by one
`acos_tap {id:"inc"}` and followed by an `acos_observe_tree` verify (all 21
resets `state_reset`+`data_cleared`, newer `frame_seq`, post-reset text `0`).

| phase | what is inside | p50 | p95 | min | max | spread |
|---|---|---:|---:|---:|---:|---:|
| serve dispatch + reply (residual) | client wall − engine total: socket read, reply write | 0.7 | 1.2 | 0.3 | 1.2 | — |
| **guest stop (signal → exit reaped)** | `close`, `try_wait` 50 ms poll, `SIGKILL` backstop, `/proc` group scan | **105.0** | 110.0 | 55.0 | 110.0 | 52 % |
| app-data wipe | **none exists**: each launch gets a *fresh* run dir (`run-dir`+`app-data` mkdir, 0–10 ms, inside launcher start) | 0 | 0 | 0 | 0 | — |
| new `acos-art` launcher start (spawn→entry) | fork/exec, arg parse, `ArtRoot` resolve | 2.0 | 3.0 | 1.0 | 3.0 | — |
| sandbox prep (entry→`guest-exec`) | linkerconfig/classpath hits, guest dex, shims, APK stage, native libs, overlays, props/logd, peer+`ServiceHost`, bwrap argv | 10.0 | 10.0 | 0.0 | 10.0 | — |
| bwrap spawn (`guest-exec`→`guest-spawned`) | `spawn_scoped` + cgroup scope + `pre_exec` | 20.0 | 30.0 | 10.0 | 30.0 | — |
| **guest spawn → first Java line** | bwrap mounts/namespaces, seccomp, `dalvikvm64` exec, **ART runtime init (boot image map/relocate)**, `LaunchMain` class load | **79.5** | 114.9 | 71.9 | 122.8 | 54 % |
| ART `initAndroidRuntime` (java→`android-runtime ready`) | `AndroidRuntime` start, JNI, native registration | 3.1 | 4.3 | 2.9 | 4.8 | 45 % |
| framework boot → `attach` returned | `Looper.prepareMainLooper`, `new ActivityThread`, `startBinderLooper`, bind/display/appInfo/instrumentation parcels, `attachApplication` | 8.0 | 10.2 | 7.2 | 11.9 | 38 % |
| `Application.onCreate` (BIND_APPLICATION msg 110) | `handleBindApplication` → `LoadedApk` → `makeApplication` → `Instrumentation.callApplicationOnCreate` | 41.1 | 76.4 | 38.4 | 76.7 | 93 % |
| `application-created` marker | watcher sees `mInitialApplication` (5 ms poll) | 2.4 | 5.0 | 0.1 | 5.1 | — |
| Activity launch (LAUNCH_ACTIVITY msg 159) | `ClientTransaction` → `performLaunchActivity` + resume | 10.6 | 18.1 | 8.1 | 22.3 | 94 % |
| **post-resume fixed sleep** | `Thread.sleep(200)` "let the fixture's onResume finish" | **201.7** | 204.0 | 199.8 | 204.9 | 2 % |
| first traversal + live probe | `resumed` → `live-probe-started` (probe sees the drawn tree) | 0.2 | 0.2 | 0.0 | 0.2 | — |
| ready line (C1 generation > 0) | host waits `activity-resumed` + probe sample + `generation>0`, 10 ms poll (`launch.rs:2032`) | 13.7 | 17.8 | 7.4 | 18.1 | 76 % |
| **frame bind (`tree` cmd)** | 150 ms quiet window + `capture_c1` (PNG 60.3 ms) + probe/tree JSON | **238.0** | 252.0 | 233.0 | 271.0 | 8 % |
| **total (client)** | reset request → reply with a fresh bound frame | **702.6** | 805.8 | 639.7 | 813.4 | 23.6 % |

Chain from the guest side (per sample): spawn → java 80 → ART ready 83 → attach
92 → BIND_APPLICATION finish 132 → `application-created` 134 → LAUNCH_ACTIVITY
finish 144 → `activity-resumed` 345 → ready line 360. Then +238 bind +105 stop
+32 launcher ≈ 702. Nothing large is hidden: the chain sums to the total within
2 %.

## 3. Dominant phases, by file:line

1. **Frame bind, 238 ms.** `ArtSession::reset` leaves "a live frame bound" by
   sending a full `tree` command (`art.rs:1899–1902`; serve's reset also calls
   `ensure_bound_tree`, `serve.rs:1383`). In the launcher `tree`
   (`launch.rs:2167`) first calls `wait_for_settled_observation`
   (`launch.rs:1868`, `let quiet = Duration::from_millis(150)` at `:1878`),
   then `capture_c1` (`:2211`, `:1688`) which composites, **PNG-encodes the
   full 1080×2400 buffer** (`:1703`) and writes it to disk, then serialises the
   probe tree. None of the PNG is used by the reset reply.
2. **Post-resume sleep, 202 ms.** `LaunchMain.java:1276`
   (`Thread.sleep(200)`) inside the "wait for mResumed" watcher, executed on
   every `--until activity-resumed` launch, therefore on every reset.
3. **Guest stop, 105 ms.** `ArtSession::shutdown` (`art.rs:1490`) polls
   `child.try_wait()` every 50 ms (`:1507`) before the `SIGKILL` backstop
   (`:1520`). The `acos-art` close path normally takes ~5 ms, so ~100 ms of
   this is the poll quantum.
4. **Guest spawn → first Java line, 80 ms** — the price of having no zygote:
   bwrap mounts/seccomp, `execve(dalvikvm64)`, ART boot-image map/relocate and
   `LaunchMain` class load (`launch.rs:647` spawn, `guest-exec`/`guest-spawned`
   stages at `:646`/`:697`).

## 4. Sources of the spread (14.1 % in W57; 23.6 % here)

Fastest sample 639.7 ms vs slowest 813.4 ms (+174 ms). Phase-by-phase delta:

| phase | fast | slow | Δ | share of gap |
|---|---:|---:|---:|---:|
| guest stop | 55 | 109 | **+54** | 31 % |
| guest spawn → java (bwrap+ART+class load) | 73.8 | 114.9 | **+41** | 24 % |
| `onCreate` (BIND_APPLICATION) | 40.8 | 76.4 | **+36** | 21 % |
| LAUNCH_ACTIVITY msg | 9.0 | 22.3 | +13 | 8 % |
| frame bind | 233.0 | 252.0 | +19 | 11 % |
| ready wait | 8.3 | 14.9 | +7 | 4 % |
| bwrap spawn, fw attach, art init, sleep | 20/8.9/3.0/201 | 30/11.9/4.8/204 | +17 | 10 % |

1. **The 50 ms stop quantum is the single largest discrete jump** and it is
   bimodal, not noisy: stop ∈ {55, 56, 57} for 4/18 samples and
   {105, 106, 109, 110} for 14/18. That alone is a 50 ms (7 %) step between two
   otherwise identical resets.
2. **CPU/scheduler contention** on the guest's critical path shows up in the two
   boot phases that run the most guest code (`spawn→java` and
   `BIND_APPLICATION`), and they move together: the slow sample is slow in both
   (+41 and +36). Host load1 rose to 6.58 during rep 2 and two samples there are
   the 775–813 ms outliers; `capture_c1`'s composite also jumps 10→31 ms under
   that load (201 % spread).
3. **10 ms poll quanta** in the two readiness gates (ready line,
   `launch.rs:2032`; `application-created` watcher, 5 ms) add 0–10 ms each and
   are the reason `ready wait` is 7–18 ms.
4. **10 ms clock granularity** of the stage markers (`exec_elapsed_ms`,
   `lib.rs:1276`) quantises the small launcher stages; irrelevant to the total
   but it is why `sandbox prep` shows 0 vs 10.
5. Not a source: dispatch + reply is 0.7 ms p50 (1.2 max); the log flush and
   engine wrapper are ~1 ms. The spread is entirely inside stop + guest boot +
   frame bind, i.e. the guest and the host scheduler, not the daemon.

## 5. What legacy ACVM's `reset --app` did

`acvm_lib/redroid_backend.py::_reset_app` (`:2629`) and `commands.py::reset`
(`:348`), measured by `bench/d2_reset_bench.py` (`iter/D2.md` §1):

```
capture granted runtime permissions   (dumpsys package)
am force-stop --user U pkg            adb shell
pm clear     --user U pkg             adb shell   (must print "Success")
pm grant     --user U pkg <perm>...   adb shell   (re-grant, usually empty)
am start     --user U --display D -n pkg/.Activity
poll dumpsys activity activities every 0.15 s until "visible=true" for (pkg,U,D)
```

* **110.3 ms** is the `reset --app` *command*: 4–6 `adb shell` round trips on an
  already-booted system (the whole session/display/user is untouched). It is an
  orchestration cost, not a boot.
* **748.1 ms** is the benchmark's `reset` → "first non-black frame": the app is
  restarted by the platform. `am start` asks `system_server`/zygote for a new
  app process, which **forks from the already-booted zygote**, so the new
  process inherits the initialised ART runtime, the mapped boot image and the
  preloaded framework classes. Legacy never pays ART init, boot-image
  relocation or framework boot; it pays process fork + `bindApplication` +
  Activity launch + first draw, and the benchmark's own readiness loop is a
  **0.2 s poll of `shot`** (`d2_reset_bench.py:47`), plus `_wait_window_visible`
  sleeps 0.15 s between `dumpsys` calls.
* Consequence for the comparison: legacy's 748 ms contains roughly 200–350 ms of
  poll/adb quantisation, so its true reset cost is plausibly ~450–550 ms — the
  same order as ACOS's 702 ms. ACOS pays ~100 ms more *because it is honest
  about a fresh runtime* (bwrap+ART+framework boot, §3 item 4) and ~240 ms of
  *avoidable* work (frame bind + fixed sleep).

## 6. Candidate improvements (not implemented), ranked

Savings are against this table's p50; risks are correctness/isolation, not
schedule.

| # | change | expected saving | risk |
|---|---|---:|---|
| 1 | **Replace `Thread.sleep(200)` with a real readiness condition** (`mCalled && mResumed`, the fixture's onResume log line, or first `ViewRootImpl` traversal) — `LaunchMain.java:1276` | **−195…−200 ms** (702→~505) | Low. The sleep exists so the fixture's `onResume()` body/log finishes before the marker (W15 lifecycle assertions). Replacing it with a condition is *more* honest, but the marker would otherwise race the app's log in rare cases. No state or isolation effect. |
| 2 | **Bind the frame, not the tree**: reset only records the C1 generation/seq (the ready line already proves `generation>0` and `probe_seq≥1`), and read the tree lazily on the next tree-consuming verb; drop the 150 ms quiet window for reset (`art.rs:1899`, `launch.rs:1878`/`:2167`) | **−235 ms** (702→~467) if the whole `tree` goes; **−150 ms** if only the quiet window is skipped | Medium-low. `serve.rs::reset` documents "a live frame bound" so an `assert_text` can run immediately; lazy binding keeps correctness (the assert reads a fresh tree) but changes one round trip and the reply no longer carries a tree-derived `frame_seq` (use the buffer seq). No isolation effect. |
| 3 | **Block on the child instead of polling**: `waitpid`/`child.wait()` with the same deadline, keep `SIGKILL` as the timeout path (`art.rs:1490`) | **−95 ms** p50 (105→~8; removes the 55/105 bimodality, which is most of the spread) | Low. Same "group empty" proof afterwards; only the wait primitive changes. No isolation effect. |
| 4 | **Warm-fork from a zygote** (the legacy mechanism): keep one pristine, framework-booted guest and `fork()` it per reset; the child gets fresh app-data and binds its own Activity | **−80…−120 ms** (bwrap+ART init+class load; with a post-attach spare also `attach`, −8) | **High.** Must fork a process that has *never* run an app (no static/JNI/heap carry-over), and the in-process `ServiceHost`/peer/binder handles are per-launch — a fork shares them unless re-created. COW of framework classes is safe (that is Android's design), COW of the *used* guest is a state leak. Overlaps the D-M2b shared-core study; measure RSS/COW there first. |
| 5 | **Pre-warmed spare guest (USAP-like pool)**: keep the next guest launched while idle, so reset = stop + swap + bind | −360 ms of *boot* (spawn→ready) but only if it can be done without touching the frame bind; realistically **−180…−360 ms** | **High.** Idle memory: M1 already 117.5 MiB and M2 107.5 MiB/session (both worse than legacy); a spare roughly doubles idle guest memory and would regress M1/M2 to fix M6. Also two guests alive at once breaks the "one ART guest at a time" invariant. |
| 6 | **In-process app restart via AMS** (`force-stop` + relaunch inside the same ART runtime) | **−90…−130 ms** (keeps ART init + framework attach + `Application`), plus whatever the frame bind/sleep changes give | **Disqualifying for reset.** ACOS's guest *is* the app process: the "fresh-guest-process" label and `data_cleared` claim would become false. It leaks static fields, `System` properties, loaded native libs, JIT/GC state, the old `ActivityThread`, PMS/ATMS registrations and logd state across resets — exactly the cross-reset state M6 exists to rule out. Acceptable only as a separate verb with an explicit `isolation:"same-process"` label. |
| 7 | Overlap the stop with the *launcher's* own preparation (APK copy, overlays, peer setup — ~12 ms) | −10…−20 ms | Low, but it momentarily has two guests' host-side artefacts alive; the sandbox/group must still be serialised. Small. |

Ranking by value/risk: **1, then 3, then 2** are all low-risk, none changes
isolation, and 1+2 or 1+3 already clears the 640 ms gate. 4/5 are the only
options that remove the *honest* runtime cost, and both need the D-M2b
shared-core/RSS study before they are credible; 6 is not a reset.

## 7. Instrumentation and evidence

Two trace-only, env-gated sinks were added to the shared tree (patch:
`evidence/D-M6/dm6-traces.patch`, not committed — D-M5 is editing the same
files):

* `DM6_RESET_TRACE=<file>` (`crates/acos-cli/src/engine/art.rs`): one JSON line
  per `spawn`/`ready`/`reset` with `stop_ms`, `start_ms`, `bind_ms`,
  `engine_total_ms` and host `unix_ms`. Inert when unset.
* `DM6_ART_STAGE_TRACE=<file>` (`crates/acos-art/src/lib.rs::stage`): the
  existing `ACOS_ART_STAGE_TIMING` markers also appended with pid + host
  `unix_ms` (serve keeps only an 8 KiB stderr tail, so stderr cannot be used).
* Guest phases come from the existing `ACOS_ART_INPUT_TRACE` (W59) `GUEST`
  lines (`ACOSLAUNCH MARK …` and the framework's own `Looper` message trace);
  capture phases from D-M5's `DM5_TRACE`.
* Harness `evidence/D-M6/dm6.py` (scratch copy; also at `.cache/dm6/dm6.py`).

Evidence: `results/art-runtime/evidence/D-M6/` — `run.json`, `phases.json`,
`table.json`, `binaries.sha256`, `dm6.py`, `dm6-traces.patch`, and per rep
`rep0{0,1,2}/{rep.json,reset.log,stage.log,input.log,dm5.log,serve.log}`.
C1 PNG captures (37 MiB/rep) were not copied.

`cargo test -p acos-cli -p acos-art` was **not** run for a commit because no
crate change is committed (the trace hunks stay as the evidence patch; the
shared tree also carries D-M5's in-flight hunks).
