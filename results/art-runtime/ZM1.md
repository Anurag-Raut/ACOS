# ZM1 — zygote-forked app child: how much inherited memory stays CoW-shared, and how fast the child's private memory grows

**Measurement only. No production code changed.** Worktree `/home/Qui/.paseo/worktrees/3oxa6xao/zm1`
(branch `arch-zm1-cow`, based on `main` incl. the Z4 seam). All new code is under
`research/zygote-proto/zm1/` (`drive.py`, `smaps.py`, `analyze.py`, a copy of Z3/Z4's
`ZygoteApp.java` with a research-only GC-on-command watcher, `build.sh`). Evidence:
`results/art-runtime/evidence/ZM1/` (per-child, per-category MiB) plus the raw `smaps` dumps
under `.cache/zm1/{z2,n2}/raw/`.

Every ART run went through
`tools/art-runtime/run-exclusive.sh --task ZM1 -- tools/guarded.sh --mem 12G --time 840|780 -- …`;
each locked run stayed ≤ 15 min; scratch stayed in `/home/Qui/Projects/acos/.cache/zm1` (never
`/tmp`); the host stayed well under 90 % RAM. **Time box: overrun** (the ART lock was heavily
contended — queue waits of 10–20 min per run; ~3 h wall clock in total).

---

## 0. Verdict

| question | answer |
|---|---|
| How much inherited memory is CoW-shared per child at fork? | **25.4 MiB of writable anon pages per child**: 25.05 MiB of LZ4-decompressed boot-image anon (`[anon:dalvik-…/boot-*.art]`), 0.20 MiB zygote space, 0.08 MiB class/intern/linear-alloc, 0.01 MiB non-moving space. Another **13.4 MiB/child is file-backed** (`.art`/`.oat`/`.vdex` page cache) and shared by definition. |
| Does it stay shared over a session? | **Yes, essentially completely.** After 10 taps: **25.35 MiB still shared** (−0.06), private part +0.08. After 45 taps: identical. After an explicit GC: **0.00** change in the inherited set. After 3 min idle: identical. **Erosion over the whole session: 0.08 MiB/child = 0.3 %** of the shared set. |
| What is the marginal cost of a session? (private anon+shmem per child) | **zygote 41.0 MiB at first frame, 13.2 MiB after 45 taps, 15.9 MiB after explicit GC + 3 min idle** vs **normal 71.8 / 77.3 / (79.3 at cp4)**. The saving is **43 % at first frame and 80–83 % once the session settles** — it does not erode, it *widens*. |
| Why is the normal path so much more expensive? | Two independent effects: (a) without a fork the LZ4 image is decompressed **per process** and is **26.34 MiB private dirty/session** (B2's number, reproduced here); with a fork it is one copy shared by parent + children. (b) The served path is **imageless** in the `-Ximage` sense, so framework classes/live objects land in the child's Java heap: normal region space **29.9 → 34.7 MiB/child**, zygote child **5.9 MiB/child** after its post-fork GC (the image holds that state). |
| Where does the child's private memory actually grow? | Not in the inherited set. It grows in the child's own **region space / main alloc space** (the app heap: 5.7→5.9 MiB after settling; +2.7 MiB transiently during an explicit copying GC), **gralloc memfd** (+0.7 MiB after taps), **native malloc** (+0.03 MiB), and **app class-init writes into the image** (+0.06 MiB). |
| Does a GC move objects out of the zygote space and break sharing? | **No.** `ZygoteSpace::CanMoveObjects()` is `false` (`zygote_space.h:78`), the mark bits are pre-set pre-fork (`zygote_space.cc:60`) and image/zygote spaces use mod-union tables instead of the shared card table (`heap.cc:734`, `:3435`). Measured: explicit double GC left the inherited set's Pss/SD/PD unchanged. |
| Does the 29 % Z4 saving hold over a session? | **The mechanism is confirmed and understated by Z4's checkpoint**: Z4 sampled “held after the tap” (≈ this report's cp1/cp2), where the child still holds ~32 MiB of launch garbage in its region space. ART's post-fork deferred GC reclaims it (heap.cc:4692-4735, `kPostForkMaxHeapDurationMS=2000` at `heap.cc:202`), so the steady-state saving is larger. |

---

## 1. Method

### 1.1 Harness (Z4's, unchanged seams)

* **zygote path**: one `acos-art launch` with `ACOS_ZYGOTE_APP=<.cache/zm1-build>` and
  `ACOS_ZYGOTE_N=2`; the zygote (Z3/Z4 `ZygoteApp`, copy in `research/zygote-proto/zm1`)
  pre-inits the app-independent framework, `ZygoteHooks.gcAndFinalize()`, `preFork()`, forks
  one child per session; each child applies its own namespaces/binds/guard and re-enters the
  **unmodified** `LaunchMain.main(argv_k)`.
* **normal path**: N independent `acos-art launch` processes, no `ACOS_ZYGOTE_*` variables.
* Fixture: `fixtures/build/apps/counter.apk`; tap target `:id/inc`, read back `:id/count` on a
  *settled* frame. Both sessions reached count 45 in the recorded run, so the taps are real use.
* N=2 in both paths; one host (32 CPUs), one fixture, one repetition of the checkpoint matrix.

### 1.2 Checkpoints

| # | label in the report | what it is |
|---|---|---|
| 1 | `1_after_first_frame` | ~0.5 s after both sessions reported `activity-resumed` + first settled frame |
| 2 | `2_after_10_taps` | after 10 real taps per session, each re-read from the tree |
| 3 | `3_after_45_taps` | after the tap loop (see §1.3) |
| 4 | `4_after_gc` | zygote: explicit `Runtime.gc(); runFinalization(); Runtime.gc()` in each child, driven by a research-only watcher thread (`<sessions>/<k>/gc` → `gc.done`). **Normal path: no such hook exists without a product change**, so its cp4 is sampled at the same wall-clock point after natural GCs only (its `region space` grew +1.9 MiB). |
| 5 | `5_after_idle` | after 180 s with no input (JIT, profile saver, background GC) |
| 6 | dialog | **not run** (time box; the tap-rate problem of §1.3 made a second fixture matrix infeasible). |

### 1.3 The tap count: 45, not 100 — and why it does not change the answer

The `tap` command waits for a *settled* observation, and the product's software draw path
(`ViewRootImpl.drawSoftware` → `Surface.nativeLockCanvas`) stalls if taps outrun the host
compositor's buffer release. In the pilot run the app's main thread was found stuck in
`nativeLockCanvas` (`ACOSPROBE-STALL` at ~frame 3000) after ~2.5 min of tapping; the drive
loop then blocked on a tap reply until the lock guard killed the run. The driver was hardened
(45 s per-reply timeout, stop tapping a stalled session, 150 s tap budget per run) and 45 taps
per session were achieved in both paths. **The memory tables are flat from cp2 on**, so 100 vs
45 taps cannot change the conclusion; say “45 taps” wherever the brief asked for 100.

### 1.4 Sampling and categories

For every app child and for the zygote parent at every checkpoint the driver read the **full**
`/proc/PID/smaps` (raw gzip kept) and aggregated every mapping into the brief's categories by
name/path (`smaps.py`). `Shared_Clean`/`Shared_Dirty` are the *full* mapping sizes (smaps
semantics, counted in every mapper); `Pss` is the apportioned share. Roles are derived from
`comm` — note that `/proc/comm` is 15 bytes, so the app child appears as `ixtures.counter`
(first byte dropped), and its argv still says `ZygoteApp` (the child never `exec`s).

**Marginal per session** is defined as the memory one additional session adds to the machine
once the shared core exists: **`Private_Dirty + Private_Clean + Pss_Shmem` per child**
(private anon + shmem). File-backed page cache (framework jars, fonts, `.so`, image files) is
excluded because it is host-fixed and already counted in `BASELINE.md` (103–113 MiB for the
whole host); this is the same distinction D-M2 makes with cgroup `anon+shmem`.
For Z4 comparability the guest-tree `ΣPss` is also reported (§5.3), but it includes the
apportioned shared core and is *not* a per-session cost.

### 1.5 Runs

| run | status |
|---|---|
| zygote N=2, checkpoints 1–5 | **complete** (`z2.json`, `raw/cp1..cp5`) |
| normal N=2, checkpoints 1–4 | **complete from raw smaps**; the locked run hit its 840 s guard during the 3 min idle, so `cp5` was not sampled in that run the normal-only re-run did not reach the shared ART lock inside the time box (30 min queue wait) and was cancelled |
| pilot zygote N=2 (driver v1, role labels wrong, cp1/cp2 only) | kept as `evidence/ZM1/pilot-cp1-cp2.json`; it is what first showed the cp1→cp2 post-fork GC drop |

---

## 2. Zygote path — per-category, **per child** (MiB)

Cell format: `Pss / Shared_Clean / Shared_Dirty / Private_Dirty`. Two children were sampled;
the numbers below are the aggregate divided by 2 (the two are within 0.1 MiB of each other at
every checkpoint). `cap1..cap5` = checkpoints 1–5.

| category | cap1 | cap2 | cap3 | cap4 | cap5 |
|---|---|---|---|---|---|
| boot image (`boot*.art` anon + `.art/.oat/.vdex` files) | 10.21 / 12.97 / 25.11 / 1.23 | 10.46 / 13.43 / 25.05 / 1.29 | 10.46 / 13.43 / 25.05 / 1.29 | 10.46 / 13.43 / 25.05 / 1.29 | 10.46 / 13.43 / 25.05 / 1.29 |
| zygote space | 0.10 / 0 / 0.21 / 0.07 | 0.11 / 0 / 0.20 / 0.08 | 0.11 / 0 / 0.20 / 0.08 | 0.11 / 0 / 0.20 / 0.08 | 0.11 / 0 / 0.20 / 0.08 |
| non-moving space | 0.02 / 0 / 0.01 / 0.02 | 0.03 / 0 / 0.01 / 0.03 | 0.03 / 0 / 0.01 / 0.03 | 0.03 / 0 / 0.01 / 0.03 | 0.03 / 0 / 0.01 / 0.03 |
| **region space / main heap** (+ card table, live bitmap) | 32.72 / 0 / 1.97 / 32.44 | 5.90 / 0 / 1.40 / 5.70 | 3.16 / 0 / 1.40 / 2.96 | 5.89 / 0 / 1.40 / 5.69 | 5.89 / 0 / 1.40 / 5.69 |
| large objects | 0.08 / 0 / 0 / 0.08 | 0.01 / 0 / 0 / 0.01 | 0.01 / 0 / 0 / 0.01 | 0.01 / 0 / 0 / 0.01 | 0.01 / 0 / 0 / 0.01 |
| class tables / intern / linear alloc | 0.77 / 0 / 0.08 / 0.76 | 0.78 / 0 / 0.08 / 0.77 | 0.78 / 0 / 0.08 / 0.77 | 0.78 / 0 / 0.08 / 0.77 | 0.78 / 0 / 0.08 / 0.77 |
| JIT code cache / metadata / profile | 0.00 | 0.02 / 0 / 0.02 / 0.00 | 0.02 / 0 / 0.02 / 0.00 | 0.02 / 0 / 0.02 / 0.00 | 0.02 / 0 / 0.02 / 0.00 |
| ART runtime libs (`libart`, `libdexfile`, …) | 1.95 / 6.34 / 0.12 / 0.01 | 1.99 / 6.46 / 0.12 / 0.01 | 1.99 / 6.46 / 0.12 / 0.01 | 1.99 / 6.46 / 0.12 / 0.01 | 1.99 / 6.46 / 0.12 / 0.01 |
| dex/apk/jar/oat/vdex (file) | 12.17 / 33.29 / 0 / 0 | 12.50 / 34.12 / 0 / 0 | 12.50 / 34.12 / 0 / 0 | 12.50 / 34.12 / 0 / 0 | 12.49 / 34.10 / 0 / 0 |
| gralloc / memfd / ashmem | 0.06 / 0.07 / 0.02 / 0.05 | 0.23 / 0.07 / 0.02 / 0.21 | 0.80 / 0.07 / 0.02 / 0.78 | 0.80 / 0.07 / 0.02 / 0.78 | 0.80 / 0.07 / 0.02 / 0.78 |
| native malloc (scudo/`[heap]`) | 5.65 / 0 / 1.11 / 5.51 | 5.67 / 0 / 1.08 / 5.53 | 5.68 / 0 / 1.07 / 5.54 | 5.68 / 0 / 1.07 / 5.54 | 5.68 / 0 / 1.07 / 5.54 |
| thread stacks | 0.22 / 0 / 0 / 0.22 | 0.19 / 0 / 0 / 0.19 | 0.19 / 0 / 0 / 0.19 | 0.19 / 0 / 0 / 0.19 | 0.19 / 0 / 0 / 0.19 |
| fonts | 7.50 / 14.88 / 0 / 0 | 7.50 / 14.88 / 0 / 0 | 7.50 / 14.88 / 0 / 0 | 7.50 / 14.88 / 0 / 0 | 7.50 / 14.88 / 0 / 0 |
| ICU | 1.32 / 3.90 / 0.20 / 0 | 1.31 / 3.89 / 0.20 / 0 | 1.31 / 3.89 / 0.20 / 0 | 1.30 / 3.88 / 0.20 / 0 | 1.30 / 3.88 / 0.20 / 0 |
| other `.so` | 7.65 / 22.97 / 4.25 / 0.12 | 7.72 / 23.30 / 4.18 / 0.13 | 7.72 / 23.30 / 4.18 / 0.13 | 7.72 / 23.30 / 4.18 / 0.13 | 7.72 / 23.30 / 4.18 / 0.13 |
| other dalvik anon | 0.03 / 0 / 0 / 0.03 | 0.15 / 0 / 0 / 0.15 | 0.15 / 0 / 0 / 0.15 | 0.09 / 0 / 0 / 0.09 | 0.03 / 0 / 0 / 0.03 |
| other | 0.50 / 0.67 / 0.59 / 0.32 | 0.50 / 0.68 / 0.57 / 0.32 | 0.50 / 0.68 / 0.57 / 0.32 | 0.50 / 0.68 / 0.57 / 0.32 | 0.50 / 0.68 / 0.57 / 0.32 |
| **TOTAL** | **80.95 / 95.09 / 33.68 / 40.86** | **55.04 / 96.84 / 32.91 / 14.42** | **52.89 / 96.84 / 32.91 / 12.26** | **55.55 / 96.82 / 32.91 / 14.93** | **55.48 / 96.81 / 32.91 / 14.87** |

The zygote parent itself is constant: Pss 55.7 → 55.2 MiB, Private_Dirty **0.54 MiB**, and
36.3 → 36.25 MiB `Shared_Dirty` (one physical copy, apportioned over parent + 2 children +
4 `z4ns`/`z4init` helpers). Its `boot_image` is 6.73 MiB Pss / 26.31 MiB `Shared_Dirty`;
its `dex_apk` 26.6 MiB Pss is file page cache. It does not grow with the session.

**Read the region-space row first.** The 32.4 MiB private at cap1 is launch garbage; ART's
post-fork deferred GC (a `ReduceTargetFootprintTask` at 2 s, `heap.cc:4705-4735`) drops it to
~5.7 MiB by cap2, and it stays there. The explicit GC raises it to 5.7 again (a copying GC
needs a from-space). This is the child's *own* heap, not inherited memory.

---

## 3. Normal path — per-category, **per child** (MiB)

Same cell format, same fixture, same driver, cp1–cp4 (cp5 not sampled; see §1.5).

| category | cp1 | cp2 | cp3 | cp4 |
|---|---|---|---|---|
| boot image | 33.55 / 13.56 / 0.00 / **26.34** | 33.69 / 13.94 / 0.00 / **26.34** | 33.69 / 13.94 / 0.00 / **26.34** | 33.69 / 13.94 / 0.00 / **26.34** |
| non-moving space | 0.02 / 0 / 0 / 0.02 | 0.02 / 0 / 0 / 0.02 | 0.02 / 0 / 0 / 0.02 | 0.02 / 0 / 0 / 0.02 |
| region space / main heap | 29.85 / 0 / 0 / 29.85 | 33.55 / 0 / 0 / 33.55 | 32.77 / 0 / 0 / 32.77 | 34.69 / 0 / 0 / 34.69 |
| large objects | 0.03 / 0 / 0 / 0.03 | 0.01 / 0 / 0 / 0.01 | 0.01 / 0 / 0 / 0.01 | 0.01 / 0 / 0 / 0.01 |
| class tables / intern / linear alloc | 0.83 / 0 / 0 / 0.83 | 0.84 / 0 / 0 / 0.84 | 0.84 / 0 / 0 / 0.84 | 0.84 / 0 / 0 / 0.84 |
| JIT | 0.00 | 0.01 / 0 / 0.02 / 0.00 | 0.01 / 0 / 0.03 / 0.00 | 0.01 / 0 / 0.03 / 0.00 |
| ART runtime libs | 3.72 / 7.14 / 0 / 0.13 | 3.72 / 7.14 / 0 / 0.13 | 3.72 / 7.14 / 0 / 0.13 | 3.72 / 7.14 / 0 / 0.13 |
| dex/apk/jar/oat/vdex (file) | 20.54 / 39.45 / 0 / 0 | 20.85 / 40.14 / 0 / 0 | 20.88 / 40.24 / 0 / 0 | 20.88 / 40.24 / 0 / 0 |
| gralloc / memfd | 0.12 / 0.10 / 0 / 0.07 | 0.29 / 0.10 / 0.06 / 0.21 | 0.87 / 0.10 / 0.07 / 0.79 | 0.87 / 0.10 / 0.07 / 0.79 |
| native malloc | 5.51 / 0 / 0 / 5.51 | 6.02 / 0 / 0 / 6.02 | 6.66 / 0 / 0 / 6.66 | 6.66 / 0 / 0 / 6.66 |
| thread stacks | 0.23 / 0 / 0 / 0.23 | 0.23 / 0 / 0 / 0.23 | 0.23 / 0 / 0 / 0.23 | 0.23 / 0 / 0 / 0.23 |
| fonts | 7.92 / 14.07 / 0 / 0 | 7.92 / 14.07 / 0 / 0 | 7.92 / 14.07 / 0 / 0 | 7.92 / 14.07 / 0 / 0 |
| ICU | 2.68 / 5.06 / 0 / 0.20 | 2.68 / 5.06 / 0 / 0.20 | 2.68 / 5.06 / 0 / 0.20 | 2.68 / 5.06 / 0 / 0.20 |
| other `.so` | 23.54 / 38.98 / 0 / 4.30 | 24.43 / 40.88 / 0 / 4.30 | 24.43 / 40.88 / 0 / 4.30 | 24.43 / 40.88 / 0 / 4.30 |
| other dalvik anon | 0.03 / 0 / 0 / 0.03 | 0.14 / 0 / 0 / 0.14 | 0.14 / 0 / 0 / 0.14 | 0.14 / 0 / 0 / 0.14 |
| other | 1.32 / 0.72 / 0 / 0.89 | 1.32 / 0.72 / 0 / 0.89 | 1.32 / 0.72 / 0 / 0.90 | 1.32 / 0.72 / 0 / 0.90 |
| **TOTAL** | **129.86 / 119.08 / 0.00 / 68.42** | **135.73 / 122.05 / 0.09 / 72.90** | **136.20 / 122.15 / 0.09 / 73.34** | **138.12 / 122.15 / 0.09 / 75.25** |

The normal path has **no shared writable anon set at all** (`Shared_Dirty` 0.00–0.09): every
process decompresses its own image copy. That 26.34 MiB/session of `boot_image` private dirty
is exactly the cost a fork removes.

---

## 4. CoW erosion and private growth

### 4.1 The inherited set, per child

Inherited set = `boot_image` + `zygote_space` + `nonmoving_space` + `class_meta`. (The
`boot_image` row mixes 25.05 MiB of CoW-shared anon with 13.4 MiB of read-only file mappings;
`Shared_Clean` is the file part, `Shared_Dirty` the CoW part.)

| checkpoint | Pss | Shared_Clean (file) | Shared_Dirty (CoW) | Private_Dirty | ΔPD vs cap1 | ΔSD vs cap1 |
|---|---:|---:|---:|---:|---:|---:|
| 1 first frame | 11.10 | 12.97 | 25.41 | 2.08 | 0.00 | 0.00 |
| 2 after 10 taps | 11.38 | 13.43 | 25.35 | 2.16 | **+0.08** | **−0.06** |
| 3 after 45 taps | 11.38 | 13.43 | 25.35 | 2.16 | +0.08 | −0.06 |
| 4 after explicit GC | 11.38 | 13.43 | 25.35 | 2.16 | +0.08 | −0.06 |
| 5 after 3 min idle | 11.38 | 13.43 | 25.35 | 2.16 | +0.08 | −0.06 |

**CoW erosion = 0.08 MiB per child over the entire session (0.3 % of the 25.4 MiB shared set),
all of it in the first ten taps.** After that the child is stable: no further image/zygote-space
pages go private, whether it taps, GCs explicitly, or idles for three minutes. The zygote
parent's own rows are identical across checkpoints, so the shared copy itself is untouched.

### 4.2 Private-dirty growth per category, per child (vs cap1)

| category | cap2 | cap3 | cap4 | cap5 |
|---|---:|---:|---:|---:|
| boot image | +0.06 | +0.06 | +0.06 | +0.06 |
| class tables/intern/linear alloc | +0.01 | +0.01 | +0.01 | +0.01 |
| region space (Java heap) | **−26.74** | **−29.48** | −26.75 | −26.75 |
| large objects | −0.07 | −0.07 | −0.07 | −0.07 |
| gralloc/memfd | +0.16 | **+0.73** | +0.73 | +0.73 |
| native malloc | +0.02 | +0.03 | +0.03 | +0.03 |
| other dalvik anon | +0.12 | +0.12 | +0.06 | 0.00 |

Two different things are visible and must not be conflated:

1. **Inherited-set growth is +0.07 MiB/child** (image + class metadata) — this *is* CoW
   erosion, and it is negligible.
2. **Region space falls by 26.7 MiB/child between cap1 and cap2.** That is not erosion and not
   a saving from the zygote: it is ART's post-fork deferred GC reclaiming launch garbage
   (`Heap::PostForkChildAction`: target footprint set to `growth_limit_`, then
   `ReduceTargetFootprintTask` scheduled at `kPostForkMaxHeapDurationMS` = 2 s and again at
   8 s, `heap.cc:4692-4735`, `heap.cc:202`). A session's *steady state* is ~5.7 MiB of heap,
   not 32.4. Checkpoint 1 is therefore the least favourable moment to quote a per-session cost.

The explicit GC (§1.2) is a *per-session* cost, not erosion: it raises each child's region
space from 2.96 to 5.69 MiB (+2.7 MiB) because the CC collector needs a from-space, and it
returns nothing to the inherited set (0.00 change). Three minutes of idle afterwards changes
nothing (5.69 MiB, gralloc 0.78, image +0) — no JIT growth, no profile-saver heap, no
background-GC drift in this fixture.

### 4.3 Same deltas on the normal path (per child)

`boot_image` private dirty is **constant at 26.34 MiB** from cp1 to cp4 (it was paid at load),
region space grows 29.85 → 34.69 MiB, malloc 5.51 → 6.66, gralloc 0.07 → 0.79. So the normal
session's growth over use is the same app-side heap growth, on top of a 26.3 MiB image it never
shares.

---

## 5. Marginal cost and guest-tree totals

### 5.1 Marginal per session (private anon + shmem per child, MiB)

| checkpoint | zygote | normal | saving |
|---|---:|---:|---:|
| 1 first frame | **41.0** | **71.8** | 30.8 MiB = **42.9 %** |
| 2 after 10 taps | 14.9 | 76.4 | 61.4 MiB = 80.4 % |
| 3 after 45 taps | **13.2** | **77.3** | 64.1 MiB = **82.9 %** |
| 4 after GC | 16.1 | 79.3 | 63.2 MiB = 79.7 % |
| 5 after 3 min idle | **15.9** | *(not sampled; cp4 = 79.3)* | ≈ 63 MiB ≈ 80 % |

This is the number that answers “how fast does each child's private memory grow”: **the zygote
child's private memory is 41 MiB when it first draws, falls to ~13 MiB once ART's deferred GC
runs, and then stays within ±3 MiB for the rest of the measured session** (taps, explicit GC,
3 min idle). The normal session starts at 72 MiB and keeps growing to ~79 MiB.

The zygote path's fixed cost is the core: zygote parent 55.2 MiB Pss + 4 research helper
processes 23.4 MiB Pss (a product would not spawn `z4ns`/`z4init`; Z4 §6.1). The crossover is
still N=2 (Z4 measured it), and this report's numbers make it wider.

### 5.2 What that means for the 29.4 % Z4 result

Z4 measured a 29.4 % marginal saving (`55.9` vs `79.2` MiB/session) with a checkpoint “held
after the tap”, i.e. before the post-fork GC fully ran. That number is conservative, not
optimistic: with the session settled the zygote child's private set is ~13–16 MiB, and the gap
to the normal path is ~64 MiB/session. The saving *does not erode*; nothing in the inherited
set is lost to the app's use.

### 5.3 Guest-tree ΣPss (Z4's convention, for continuity)

| checkpoint | zygote guest tree | normal guest tree | saving |
|---|---:|---:|---:|
| 1 | 241.2 (218.0 net of the 4 helpers) | 259.4 | 7.0 % (15.9 %) |
| 2 | 189.5 (166.1) | 271.4 | 30.2 % (38.8 %) |
| 3 | 185.0 (161.6) | 272.2 | 32.0 % (40.6 %) |
| 4 | 190.6 (167.2) | 275.9 | 30.9 % (39.4 %) |
| 5 | 190.3 (166.9) | — | — |

Zygote tree = zygote + 2 app children + 4 `z4ns`/`z4init` helpers (`guest_side` in the driver);
normal tree = 2 app children. Launchers/bwrap are excluded in both, as in Z4/D-M2.

---

## 6. Why the inherited set does not erode — mechanisms (AOSP `file:line`)

**M1. What is inherited is the LZ4 image's *anonymous* copy, not a file mapping.** With a boot
image ART decompresses the image into an anonymous `PROT_READ|PROT_WRITE` map
(`MapAnonymous` + `MapFile(..., PROT_READ, MAP_PRIVATE, ...)` temp map copied into it,
`art/runtime/gc/space/image_space.cc:1021-1044`). Before a fork, every process would pay its own
copy (measured normal path: 26.34 MiB `Private_Dirty`). After a fork the parent's copy is one
physical set of pages shared copy-on-write with each child — smaps shows it as
`Shared_Dirty` 25.05 MiB/child, `Private_Dirty` 1.29. The file-backed part
(`/system/framework/x86_64/boot*.oat/.art/.vdex`, 13.4 MiB/child) is page cache, shared by
definition.

**M2. ART deliberately pre-dirties everything it can before the fork, so children do not.**
`Runtime::PreZygoteFork` runs `ClassLinker::MakeInitializedClassesVisiblyInitialized` and
`UpdateMethodsPreFirstForkVisitor` — the comment says “Update ArtMethods in the boot classpath
now to avoid having forked apps dirty the memory” (`art/runtime/runtime.cc:830-840`) — then
`Heap::PreZygoteFork` (`heap.cc:2438`) splits the mutable tables:
`SetupLinearAllocForPostZygoteFork`, `InternTable::AddNewTable` (`intern_table.cc:143-146`),
`ClassLinker::MoveClassTableToPreZygote` (`class_linker.cc:4676-4685`). Post-fork class
resolution, interned strings and linear allocations go to the child's **new** tables, so they
do not write the inherited ones. This is why the measured residue is 0.08 MiB/child: only the
classes the zygote did not pre-resolve (the fixture's `MainActivity` and its resources) dirty
image objects, and only ~1.3 MiB of image pages in total.

**M3. GC cannot move objects out of the zygote space, and the design avoids dirtying its
bitmaps.** A `ZygoteSpace` is alloc-free and non-moving: `CanMoveObjects()` returns `false`
(`art/runtime/gc/space/zygote_space.h:78`), so a compacting GC has nothing to evacuate there.
Its live/mark bitmaps are part of the shared data; AOSP pre-sets the mark bit of every live
object (`ZygoteSpace::SetMarkBitInLiveObjects`, `zygote_space.cc:60`, comment at
`zygote_space.h:38`) and uses `ModUnionTableToZygoteAllocspace` for image and zygote spaces
(`heap.cc:734-740`; card processing at `heap.cc:3435-3463`) instead of writing the shared card
table. Measured: the explicit double GC at cap4 changed the inherited set's Shared_Dirty,
Private_Dirty and Pss by **0.00 MiB**.

**M4. The region space is the child's own by construction.** Post-fork allocation happens in
the per-process region space (`[anon:dalvik-main space (region space)]`), whose pages were not
resident in the parent (the parent's region space is 0.35 MiB Pss) and whose live bitmap is
private (`[anon:dalvik-region space live bitmap]`, 1.0 MiB/child). This is where the app's live
objects and the surviving launch garbage live; it is not shared and cannot erode the inherited
set.

**M5. The post-fork GC is deferred on purpose, so first-frame memory is not steady state.**
`Heap::PostForkChildAction` sets `SetIdealFootprint(growth_limit_)` and schedules
`ReduceTargetFootprintTask` at 2 s and 8 s plus a `TriggerPostForkCCGcTask`
(`heap.cc:4692-4735`), with `kPostForkMaxHeapDurationMS = 2000` (`heap.cc:202`). That is the
mechanism behind the 32.4 → 5.7 MiB region-space drop between cap1 and cap2, and the reason a
“right after fork + first frame” snapshot overstates the marginal cost of a session by
~28 MiB.

**M6. JIT is not an erosion source here.** The zygote deletes its compiler thread pool before
the fork (`Jit::PreZygoteFork`, `art/runtime/jit/jit.cc:1530-1535`) and each child re-creates
its code cache post-fork (`Jit::PostForkChildAction`, `jit.cc:1476-1525`). The measured `jit`
category is 0.00 → 0.02 MiB/child across the whole session: with the system boot image the
framework code is AOT-compiled in the image, so the child JITs almost nothing.

**M7. The normal path's extra cost is the image, paid per process.** Without a fork, the same
`image_space.cc:1021-1027` decompression runs per session and the pages can never be shared:
26.34 MiB `Private_Dirty` per session in every checkpoint, independent of use. The `-Ximage`
flag is not what matters (the product path loads the sysroot's default image); the *fork* is.

**M8. File-backed rows in the tables are page cache, not per-session memory.** `dex_apk`
(12.5 MiB Pss/child zygote, 20.9 normal), `fonts` 7.5, `other .so` 7.7 are
`Shared_Clean`; their Pss falls as more processes map them, and `BASELINE.md` counts them once
for the host (103–113 MiB). They are not part of the marginal cost.

---

## 7. Mitigations Android itself uses, and whether they apply here

| mitigation | where it lives | does it apply / what was measured |
|---|---|---|
| **Zygote space is non-moving; mark bits pre-set; mod-union tables instead of the shared card table** | `zygote_space.h:78`, `zygote_space.cc:60`, `heap.cc:734`, `:3435` | **Already active** through Z1/Z3's `preFork`/`postForkChild`. It is why an explicit GC costs 0.00 MiB of inherited-shared memory. Keep it; do not “optimise” it away. |
| **Pre-fork class table / intern table split** (`MoveClassTableToPreZygote`, `InternTable::AddNewTable`) | `class_linker.cc:4676`, `intern_table.cc:143` | **Already active.** Cheap, and the reason post-fork class loaders/strings don't dirty the shared tables. |
| **Pre-initialise classes and methods before the fork** (`MakeInitializedClassesVisiblyInitialized`, `UpdateMethodsPreFirstForkVisitor`) | `runtime.cc:830-840` | **Active, but only for the 34 classes our prototype preloads.** The residual 0.08 MiB/child is exactly the app-specific classes; a larger preload list (`ZygoteInit.preloadClasses`, AOSP preloads thousands) is the cheap lever to drive erosion to ~0. It costs zygote start-up and private zygote memory, so it is a trade to measure, not a free win. |
| **Boot image is AOT-compiled (optionally profile-guided)** | `dex2oat`, AOSP boot image | **Partially.** The sysroot image is stock; a profile-guided image (Android's own `boot-image-profile.txt` mechanism) would move more app-visible framework code out of the child heap/JIT, but ACOS's `launch.rs` has no image/flag seam (`B2.md` §3 blocker) and our numbers show the remaining JIT cost is 0.02 MiB/session, so this is *not* the lever for this fixture. |
| **Fixed image base / no per-process relocation** | `-Xnorelocate`, image base 0x70000000 | **Not needed on the zygote path**: the parent relocates once and the children inherit the pages. It *is* needed if a future product uses a shared uncompressed image without a fork (B2 measured 49.8 MiB private dirty with relocation vs 12.7 without). |
| **Heap growth limit / target utilisation** (`dalvik.vm.heaptargetutilization`, `-XX:HeapGrowthLimit`) | `props.rs` serves stock values but `launch.rs` never turns them into ART flags (B2 §1) | **Applicable and cheap to try** for the child's *own* heap, not for CoW: a lower growth limit makes the post-fork shrink/GC happen sooner (it already happens at ~2 s here) and caps region-space residence. Expect a few MiB/session, not tens — the 5.7 MiB live set is the app, not garbage. |
| **`madvise(MADV_DONTNEED)` / heap trimming after GC** | `Heap::Trim`, ART's `madvise` of region space | **Applicable to private pages only.** It cannot “un-share” the image and must not be used on the shared anon image (it is CoW: untouched pages are already free to drop). The explicit-GC +2.7 MiB from-space is the candidate. |
| **Zygote-space compaction** (`kCompactZygote`, `ZygoteCompactingCollector`) | `heap.cc:158-159`, `:2312-2371`, `:2445` | **Active but irrelevant here**: with a boot image the pre-fork non-moving space is tiny (measured `zygote_space` 0.20 MiB/child, 0.11 MiB Pss). It would matter only in a preload-heavy, imageless configuration. |
| **One preloaded process tree, no per-session helper processes** | Z4 §6.1 | **Applies to the research seam only.** The 4 `z4ns`/`z4init` helpers inherit the image and add 23.4 MiB Pss to the guest tree at N=2 (0 marginal private, but they inflate `ΣPss`). A product zygote would not spawn them. |
| **Per-child cgroup/`memory.high` and per-child scopes** (Z4 §5d) | systemd scope | **Applies to the product**, not to CoW: it bounds the child's private growth (region space/gralloc), which is where all the growth actually is. |

---

## 8. Limits, and what was not measured

1. **45 taps, not 100.** The settled-frame tap protocol plus the software draw path's
   `nativeLockCanvas` stall (pilot: `ACOSPROBE-STALL` at ~frame 3000) caps the tap rate at
   ≈0.3 taps/s; 45/session was achieved in both paths. The tables are flat from 10 taps on, so
   the number of taps does not change the answer. A future harness needs a non-blocking tap or
   a smaller fixture frame rate to reach 100.
2. **Normal cp5** was killed by the 840 s lock guard during the idle (cp1–cp4 are complete from
   the raw smaps). A second normal-only run was queued but did not reach the shared ART lock
   inside the time box (30 min queue wait) and was cancelled, so the normal path has **no cp5**
   measurement; its cp4 value (79.3 MiB) is the last measured normal point. The zygote cp5 is
   measured.
3. **Checkpoint 6 (dialog/navigation) was not run** (time box). The dialog-menu fixture exists
   and the driver supports `--dialog`; the tap-rate problem made a second matrix infeasible.
4. **Explicit GC only on the zygote path.** The normal path has no guest GC hook
   (`B2.md` §1), so its cp4 is a natural-GC sample at the same wall clock. Do not read the
   zygote cp4→cp5 delta as “GC helps/hurts” — it is the CC from-space allocation.
5. **One fixture, one host, N=2, one repetition.** The counter is the lightest real app; a
   heavier fixture (lists, images, WebView) would grow the region space/shmem more, but that
   growth is private by construction and cannot change the CoW answer. `N=2` is enough for the
   per-child private number; `ΣPss` apportioning at N=5+ would change only the shared part.
6. **No cgroup `anon+shmem` totals and no CPU numbers** in this report (smaps only); D-M2 has
   the cgroup method and the 18.2 MiB/session gralloc shmem figure.
7. **The research seam adds** a GC-watcher thread (one stack, ≤0.2 MiB) and 4 helper processes;
   both are constant across checkpoints, so they do not affect the deltas.

---

## 9. Reproducing

```bash
# 1. build (worktree env: .cache/art, .cache/boot and fixtures/build symlinked from the main checkout)
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
CARGO_TARGET_DIR=/home/Qui/Projects/acos/.cache/zm1-target CARGO_BUILD_JOBS=4 \
  cargo build --release -p acos-art
ZBUILD=zm1-build research/zygote-proto/zm1/build.sh     # research dex + fork shim + guard copy

# 2. the two N=2 runs (one lock; ~840 s guard; the normal run needs its own if the guard hits)
tools/art-runtime/run-exclusive.sh --task ZM1 -- tools/guarded.sh --mem 12G --time 840 -- \
  bash /home/Qui/Projects/acos/.cache/zm1/run-main.sh
#   run-main.sh = drive.py --mode zygote --n 2 ... ; drive.py --mode normal --n 2 ...

# 3. tables
python3 research/zygote-proto/zm1/analyze.py \
  /home/Qui/Projects/acos/.cache/zm1/z2.json /home/Qui/Projects/acos/.cache/zm1/n2.json
```

`drive.py` writes `<out>/raw/<cpN>/*.smaps.gz` (raw), and `<out>.json` (rollups + per-category
MiB + tap/GC records). The GC-on-command hook is `<out>/sessions/<k>/gc` (written by the
driver) → `gc.done` (written by the child), implemented in this directory's `ZygoteApp.java`;
nothing outside `research/zygote-proto/zm1/` was changed.
