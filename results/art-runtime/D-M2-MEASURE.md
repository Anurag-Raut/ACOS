# D-M2 — measured facts: what one extra ART session actually costs

**Diagnosis only.** No product code under `crates/` was changed; the ART engine and
the W57 harness verdict behaviour are untouched (`tools/tests/test-d2-warm.sh`:
**15 passed, 0 failed** after this work). This file is the measurement half of
D-M2 (code inventory: D-M2b, in parallel).

* **Commit measured:** `30a693d987a01b7f301be7f68f6bdc3e7af0e8ad` (HEAD, main checkout)
* **Build:** `CARGO_TARGET_DIR=.cache/dm2-target cargo build --release -p acos-cli -p acos-art`
  (58.6 s, release, `[profile.release] debug = 1`)
* **`acos` sha256:** `3d496c47d19ae1ceea70b6358e706a90159c8d1ccf01867835acd4b9bab32c2f`
  (159,675,240 B)
* **`acos-art` sha256:** `129426e2416d9f3f196b6a86c8d92864b74961a570104f6f4af070f42e4d5ceb`
  (57,622,368 B)
* **APK:** `fixtures/build/apps/counter.apk`, sha256 in each `run.json`
* **Host:** 32 CPUs, 60 GiB RAM (17–18 GiB used before each run, MemAvailable
  41–43 GiB, never above 90 %), cgroup v2, Swap 0 for every session.
* **Harness:** `tools/art-runtime/dm2/measure.py` (new; imports
  `tools/art-runtime/d2_warm.py`'s `Serve`/`WarmSampler`/`launch_session`/host gate
  and does not change them). Derived tables: `evidence/D-M2/analysis.json`,
  `evidence/D-M2/analysis.md`.
* **Every ART run** went through
  `tools/art-runtime/run-exclusive.sh --task DM2a -- tools/guarded.sh --mem 12G --time 900|1200 -- …`.
  The five measured runs are in `.cache/art-locks/runs.log`; each took 72 s–3 m 50 s
  (queue waits up to ~3 min while other agents held the ART lock), and every run
  ended with **0 survivors** and an empty leftover scan (no `bwrap`, `dalvikvm64`
  or `acos-art` naming the rep scratch).

## 0. Method in one paragraph

For N ∈ {1, 5, 10} we start one `acos serve --engine art` daemon, launch N
`counter.apk` sessions over its socket, and **wait 60 s after the last session's
first frame** — the exact W57/M2 warm state. Then, while the sessions are held,
we dump every process in the daemon's tree: argv, ppid, comm, threads, cgroup,
namespace inodes, fd table (with memfd sizes/inodes), `/proc/PID/smaps_rollup`
and the full `/proc/PID/smaps`. Memory totals use the same type as M2:
**cgroup v2 `memory.stat` anon+shmem of every cgroup that carries the job**
(the guarded scope plus one transient scope per guest), with the harness's own
RssAnon+RssShmem subtracted (the W45/W52/W57 rule). Per-session attribution is
the process tree each launch added. `sum(Pss)` over sessions is used as the
system-wide **unique-page** total: a page mapped by k of the counted processes
contributes size/k to each, so the k contributions add to one page size (stated
substitute — see §3).

Runs kept as evidence (two reps at N=5 and N=10 after the first N=5; the detailed
exemplar below is **`n5-rep1`**, because its raw dump is the one not overwritten
by the props-capture rerun):

| run | N | cgroup anon+shmem total | M2 incremental (total−bare)/N | cgroup peak | daemon threads | survivors |
|---|---|---|---|---|---|---|
| `n1-rep0` | 1 | 117.555 MiB | 116.094 MiB/session | 177.387 MiB | 4 | 0 |
| `n5-rep0` | 5 | 574.960 MiB | **114.699** MiB/session | 705.973 MiB | 12 | 0 |
| `n5-rep1` | 5 | 570.054 MiB | **113.719** MiB/session | 703.692 MiB | 12 | 0 |
| `n10-rep0` | 10 | 1142.547 MiB | 114.109 MiB/session | 1382.352 MiB | 22 | 0 |
| `n10-rep1` | 10 | 1148.829 MiB | 114.736 MiB/session | 1388.324 MiB | 22 | 0 |

Means: **N=5 572.51 MiB (114.21 MiB/session incremental)**, **N=10 1145.69 MiB
(114.42/session)**. The daemon alone (no session) is 1.46 MiB anon+shmem,
consistent with W57's 1.355 MiB; M1 at N=1 is **117.56 MiB**, reproducing W57's
117.50 MiB.

---

## 1. The actual process trees

Every session is **four processes plus a share of the daemon**: the host-side
`acos-art` launcher (a child of `acos serve`), an outer `bwrap` still in the
host namespaces, an inner `bwrap` in the session's namespaces, and the guest
`dalvikvm64` (comm truncated to `ixtures.counter`). The guest is not a zygote:
each session boots its own ART runtime from scratch.

### N = 1 (`n1-rep0`, 60 s after first frame)

```text
acos serve [1019817]  (daemon, host ns, job scope, 4 threads, Rss 20.9 / Pss 17.0 / PD 11.6 MiB)
└── session 0
    acos-art [1019912]  ppid 1019817  host ns  job scope           9 thr   Rss 15.9  Pss 12.1  PD 7.3
    └─ bwrap  [1019921] ppid 1019912  host ns  job scope           1 thr   Rss  2.3  Pss  0.1  PD 0.0
       └─ bwrap [1019923] ppid 1019921  session ns  session scope  1 thr   Rss  1.3  Pss  0.1  PD 0.1
          └─ fixtures.counter [1019927] ppid 1019923  session ns  session scope
                                                                   17 thr  Rss 204.8 Pss 202.1 PD 80.2
```

The guest's Pss here is inflated by the `Private_Clean` accounting artefact
described in §3 (single mapper); at N≥5 it is ~100 MiB.

### N = 5 (`n5-rep1`)

Session 0 verbatim; sessions 1–4 are the same shape with different pids.

```text
acos serve [1035988] (daemon, 12 threads, Rss 61.4 / Pss 57.4 / PD 52.0 MiB)
├── session 0
│   acos-art [1036115] p1035988 host ns job scope       9 thr  Rss  16.2 Pss  8.5 PD  7.5
│   └─ bwrap  [1036124] p1036115 host ns job scope      1 thr  Rss   2.3 Pss  0.1 PD  0.0
│      └─ bwrap [1036126] p1036124 sess ns sess scope   1 thr  Rss   1.3 Pss  0.1 PD  0.1
│         └─ fixtures.counter [1036130] p1036126       17 thr  Rss 201.9 Pss 102.4 PD 76.9
├── session 1  acos-art 1036152 ─ bwrap 1036161 ─ bwrap 1036166 ─ guest 1036167
│              (16.0/8.3/7.3)      (2.2/0.1/0.0)   (1.3/0.1/0.1)   (200.8/101.4/75.9)
├── session 2  acos-art 1036189 ─ bwrap 1036198 ─ bwrap 1036203 ─ guest 1036204
│              (16.1/8.3/7.2)      (2.3/0.1/0.0)   (1.3/0.1/0.1)   (205.5/105.9/80.3)
├── session 3  acos-art 1036226 ─ bwrap 1036235 ─ bwrap 1036240 ─ guest 1036241
│              (16.0/8.2/7.2)      (2.3/0.1/0.0)   (1.3/0.1/0.1)   (204.8/105.5/80.2)
└── session 4  acos-art 1036263 ─ bwrap 1036272 ─ bwrap 1036277 ─ guest 1036278
               (15.8/8.2/7.1)      (2.3/0.1/0.0)   (1.3/0.1/0.1)   (204.5/105.4/80.2)
```

### N = 10 (`n10-rep0`)

Ten identical copies of the same four-process chain. Session 0 verbatim:

```text
acos serve [1026403] (daemon, 22 threads, Rss 111.9 / Pss 107.9 / PD 102.5 MiB)
└── session 0
    acos-art [1026590] p1026403 host ns job scope        9 thr  Rss  16.4 Pss  8.2 PD  7.7
    └─ bwrap  [1026599] p1026590 host ns job scope       1 thr  Rss   2.3 Pss  0.1 PD  0.0
       └─ bwrap [1026601] p1026599 sess ns sess scope    1 thr  Rss   1.3 Pss  0.1 PD  0.1
          └─ fixtures.counter [1026605] p1026601        17 thr  Rss 200.6 Pss 88.8 PD 76.0
```

Per-session guest (Rss / Pss / PD, MiB) at N=10, all 10:
`200.6/88.8/76.0, 205.6/93.5/80.6, 204.6/92.5/79.7, 204.6/92.9/80.2, 202.4/90.4/77.6,
202.0/89.8/77.0, 200.5/88.9/76.2, 205.8/94.0/81.1, 207.4/95.2/82.3, 205.4/93.3/80.4`.
Every launcher is 15.7–16.4 Rss / 7.7–8.2 Pss / 7.1–7.7 PD, every bwrap pair
≈3.6 Rss / 0.2 Pss / 0.1 PD.

### Namespaces, sandbox and cgroups

The daemon, the launcher and the **outer** bwrap share the host namespaces
(`mnt:[4026531832] pid:[4026531836] user:[4026531837]`). Each session gets its
own bwrap set: `pid`, `mnt`, `net`, `ipc`, `uts`, `user` and `cgroup`
namespaces (e.g. N=5 session 0: `mnt:[4026533823] pid:[4026533830]
net:[4026533842] user:[4026533895] cgroup:[4026533841]`), and the **inner**
bwrap and the guest both live in it. The guest sees only `/system`, `/apex`,
`/data`, `/proc`, `/sys`, `/dev` and two sockets (full argv in §7/raw evidence).

Cgroups (cgroup v2, one scope per session):

* `acos serve` + all N launchers + the outer bwrap: the guarded job scope
  `…/app.slice/run-p<jobpid>-i<inode>.scope`;
* each session's inner bwrap + guest: its own transient scope
  `…/app.slice/run-p<bwrap-pid>-i<inode>.scope` (created per session).

That second scope is the W57 sampler's "guest cgroup outside the job", and it is
where the 18.2 MiB of graphics buffers are charged (§5). All sessions have
**Swap = 0**; nothing is swapped.

---

## 2. Mapping-level breakdown per process

The N=5 exemplar, session 0, every process, from `/proc/PID/smaps` (MiB, groups
per §3):

### guest `fixtures.counter` [1036130]

Rss 201.90 / Pss 102.37 / Private_Dirty 76.91 / Private_Clean 0.30 /
Shared_Clean 124.59 / Shared_Dirty 0.09 / Swap 0. 17 threads, 55 fds.

| group | Rss | Pss | Private_Dirty | Private_Clean | Shared_Clean | Shared_Dirty |
|---|---|---|---|---|---|---|
| dalvik heap: main/region space | 37.05 | 37.05 | 37.05 | 0 | 0 | 0 |
| boot image (boot*.art/.oat/.vdex) | 40.17 | 29.19 | **26.34** | 0.02 | 13.82 | 0 |
| malloc (scudo/libc_malloc/[heap]) | 5.35 | 5.35 | 5.35 | 0 | 0 | 0 |
| other .so | 45.08 | 12.12 | 4.29 | 0.10 | 40.69 | 0 |
| dalvik linear alloc + other dalvik anon | 2.42 | 2.42 | 2.42 | 0 | 0 | 0 |
| other | 1.82 | 1.09 | 0.89 | 0 | 0.93 | 0 |
| art runtime libs | 10.86 | 2.43 | 0.35 | 0 | 10.51 | 0 |
| stacks | 0.24 | 0.23 | 0.23 | 0 | 0.01 | 0 |
| other memfd/ashmem | 0.14 | 0.08 | 0.04 | 0 | 0.04 | 0.06 |
| dalvik heap: non-moving space | 0.02 | 0.02 | 0.02 | 0 | 0 | 0 |
| dalvik heap: large object space | 0.01 | 0.01 | 0.01 | 0 | 0 | 0 |
| dalvik jit code cache (anon_shmem) | 0.02 | 0.01 | 0 | 0 | 0 | 0.02 |
| fonts (.ttf/.otf/.ttc) | 15.66 | 3.33 | 0 | 0.14 | 15.53 | 0 |
| icu (.dat) | 0.75 | 0.10 | 0 | 0 | 0.75 | 0 |
| resources (apk) | 6.25 | 1.25 | 0 | 0 | 6.25 | 0 |
| framework jar/dex | 36.13 | 7.32 | 0 | 0.05 | 36.08 | 0 |

Read: **76.9 MiB of the guest is private-dirty anonymous memory** — region space
(37.1), anonymous boot-image copy (26.3), malloc (5.4), linear alloc + dalvik
control structures (2.4), other/PAC (1.8), art-lib data (0.35), stacks (0.23).
Its 124.6 MiB of `Shared_Clean` (plus 36.1 MiB of framework jars) is page cache
shared with the other sessions *and* the host, and costs nothing per session.

### launcher `acos-art` [1036115] (host-side C1/compositor client)

Rss 16.17 / Pss 8.52 / PD 7.48; 9 threads.

| group | Rss | Pss | Private_Dirty | Private_Clean | Shared_Clean | Shared_Dirty |
|---|---|---|---|---|---|---|
| acos-art host/compositor (anon + binary text) | 11.61 | 7.80 | 6.83 | 0 | 4.77 | 0 |
| malloc (scudo/libc_malloc/[heap]) | 0.49 | 0.49 | 0.49 | 0 | 0 | 0 |
| other .so | 4.02 | 0.19 | 0.12 | 0 | 3.90 | 0 |
| stacks | 0.05 | 0.04 | 0.04 | 0 | 0.02 | 0 |

"acos-art host/compositor" = every mapping of an `acos-art` process that is not
a `.so`: its 4.8 MiB binary text (shared across the N launchers), its Rust/glibc
anon and heap. This is the C1 client side: **6.8 MiB private per session**;
launcher `anon+shmem` is 7.3–7.7 MiB.

### sandbox (both `bwrap` processes)

Together 3.6 MiB Rss / 0.24 Pss / 0.11 PD: 3.3 MiB shared libc (`other .so`),
0.23 MiB anon/heap/stacks. Negligible; outer and inner bwrap carry the same
argv (the inner is bwrap's post-namespace re-exec).

### daemon `acos serve`

| run | threads | Rss | Pss | Private_Dirty | unnamed `(anon)` mappings |
|---|---|---|---|---|---|
| N=1 | 4 | 20.91 | 16.97 | 11.57 | 10.33 MiB |
| N=5 (`rep0`/`rep1`) | 12 | 60.56 / 61.38 | 56.56 / 57.38 | 51.11 / 51.99 | 49.9 / 49.9 MiB |
| N=10 (`rep0`/`rep1`) | 22 | 111.87 / 111.43 | 107.87 / 107.42 | 102.50 / 102.44 | 101.3 / 101.3 MiB |

The daemon starts at 10.3 MiB anon and grows **~10 MiB and 2 threads per held
session**, all in unnamed `(anon)`; no file mapping grows. That is a
*shared-core* cost, not a guest-tree cost — and M2's `(total − bare)/N` formula
charges it to every session, while W57's per-session process attribution could
not see it (the daemon was W57's "bare" baseline). See §4.

---

## 3. Group tables, top Private_Dirty, and what is shared

Grouping rule (one rule per pathname, applied to every smaps entry):
ART libs = `libart*`/`libdexfile`/`libprofile`/`libnativebridge`/`libnativeloader`/
`libsigchain`/`libunwindstack`…; other `.so`; `boot*.{art,oat,vdex}` **including
ART's `[anon:dalvik-/system/framework/boot-*.art]` image mappings**; other
`.art/.oat/.vdex` (app dexopt); framework `.jar`/`.dex`;
`[anon:dalvik-main space (region space)]`, zygote/LO/non-moving/other spaces;
`dalvik-jit-code-cache`; linear alloc and other `dalvik-*` anon;
`[anon:scudo*]`/`libc_malloc`/`[heap]`; `acos-gralloc` and other memfd/ashmem;
fonts; resources (`.apk`); `icu*.dat`; stacks/vdso/vvar; everything anon in an
`acos-art` process = "acos-art host/compositor"; other.

Per-session values are the mean over that N's reps and sessions; `unique` = Σ Pss
across the N sessions (each physically shared page counted once), `shared` =
Σ Rss − Σ Pss. **All MiB.**

| group | N=1 PD/s | N=5 PD/s | N=10 PD/s | N=5 Rss/s | N=5 Pss/s | N=5 unique (ΣPss) | N=5 shared |
|---|---|---|---|---|---|---|---|
| dalvik heap: main/region space | 40.23 | 38.95 | 39.00 | 38.95 | 38.95 | 194.76 | 0.00 |
| boot image (boot*.art/.oat/.vdex) | 26.34 | 26.34 | 26.34 | 40.35 | 29.26 | 146.32 | 55.41 |
| acos-art host/compositor (anon) | 6.64 | 6.75 | 6.87 | 11.53 | 7.73 | 38.66 | 19.01 |
| malloc (scudo/libc_malloc/[heap]) | 5.89 | 5.81 | 5.86 | 5.81 | 5.81 | 29.06 | 0.00 |
| other .so | 4.43 | 4.43 | 4.43 | 52.31 | 12.35 | 61.75 | 199.81 |
| dalvik linear alloc + other dalvik anon | 2.30 | 2.35 | 2.37 | 2.35 | 2.35 | 11.74 | 0.00 |
| other | 0.93 | 0.93 | 0.93 | 2.04 | 1.17 | 5.85 | 4.35 |
| art runtime libs | 0.35 | 0.35 | 0.35 | 10.84 | 2.42 | 12.12 | 42.06 |
| stacks | 0.28 | 0.28 | 0.47 | 0.35 | 0.30 | 1.48 | 0.27 |
| dalvik heap: large object space | 0.13 | 0.08 | 0.06 | 0.08 | 0.08 | 0.41 | 0.00 |
| other memfd/ashmem | 0.04 | 0.04 | 0.04 | 0.14 | 0.08 | 0.41 | 0.32 |
| dalvik heap: non-moving space | 0.02 | 0.02 | 0.02 | 0.02 | 0.02 | 0.10 | 0.00 |
| dalvik jit code cache | 0.00 | 0.00 | 0.00 | 0.02 | 0.01 | 0.06 | 0.06 |
| dalvik heap: zygote space | 0.00 | 0.00 | 0.00 | 0.02 | 0.02 | 0.02 | 0.00 |
| framework jar/dex | 0.00 | 0.00 | 0.00 | 36.10 | 7.30 | 36.48 | 144.01 |
| fonts (.ttf/.otf/.ttc) | 0.00 | 0.00 | 0.00 | 15.51 | 3.28 | 16.39 | 61.15 |
| resources (apk) | 0.00 | 0.00 | 0.00 | 6.25 | 1.25 | 6.24 | 25.01 |
| icu (.dat) | 0.00 | 0.00 | 0.00 | 0.75 | 0.10 | 0.50 | 3.23 |

Top 15 by Private_Dirty per session is rows 1–15 above (everything from
`stacks` down is < 0.5 MiB). The guest's private total is stable at 76–80 MiB
regardless of N; the daemon and launcher account for the rest.

### Shared vs private totals

Process set only (daemon excluded; its 20.9–111.9 MiB are its own), means per N:

| N | Σ Rss | Σ Pss = unique | Σ shared (Rss−Pss) | Σ Private_Dirty | Σ Shared_Clean | Σ Private_Clean |
|---|---|---|---|---|---|---|
| 1 | 224.40 | 214.45 | 9.95 | 87.58 | 12.64 | 123.91 |
| 5 | 1117.05 | 562.35 | 554.70 (49.7 %) | 431.64 | 682.05 | 1.99 |
| 10 | 2237.28 | 998.11 | 1239.18 (55.4 %) | 867.45 | 1365.95 | 1.16 |

Per session at N=5 that is 223.4 MiB of address space for 112.5 MiB of unique
physical memory; per session at N=10 it is 223.7 for 99.8 MiB, because the
page-cache sharing is saturated.

**Caveat on Private_Clean.** `Private_Clean` in smaps is *not* unique memory.
In the N=1 run the read-only `framework.jar`/`boot*.oat` maps show as
`Private_Clean` (124 MiB); in the N=5 run the same pages are reported
`Shared_Clean` because another process maps them, and Pss falls from 22.3 to
4.47 MiB per session (exactly 1/5). This is the kernel's MAP_PRIVATE file-page
accounting: a page-cache page mapped by one process is "private clean", by
several "shared clean". Treat Private_Dirty as the honest per-session private
cost and **Pss as the only honest uniqueness measure**.

### Which file-backed mappings are shared, which are private copies

At N=5 (`n5-rep1`), per path (Rss/Pss/Private_Dirty/Private_Clean/Shared_Clean,
MiB; all five sessions summed):

| path | group | Rss | Pss | PD | PC | SC |
|---|---|---|---|---|---|---|
| `/system/framework/framework.jar` | framework jar | 111.6 | 22.4 | 0 | 0.04 | 111.56 |
| `/system/framework/x86_64/boot-framework.oat` | boot image | 42.9 | 9.0 | 0.02 | 0.25 | 42.61 |
| `/apex/com.android.art/lib64/libart.so` | art libs | 33.0 | 6.9 | 0.41 | 0 | 32.58 |
| `/system/framework/framework-res.apk` | resources | 31.1 | 6.2 | 0 | 0 | 31.13 |
| `/system/lib64/libhwui.so` | other .so | 28.8 | 6.5 | 0.88 | 0 | 27.89 |
| `…/.cache/dm2-target/release/acos-art` | launcher | 24.7 | 5.6 | 0.70 | 0.02 | 24.02 |
| `/usr/lib/libc.so.6` | other .so | 21.0 | 0.40 | 0.20 | 0 | 20.68 |
| `/apex/com.android.art/javalib/core-oj.jar` | framework jar | 18.1 | 3.6 | 0 | 0 | 18.05 |
| `/system/framework/x86_64/boot.oat` | boot image | 15.6 | 3.2 | 0.02 | 0.04 | 15.53 |
| `/apex/com.android.art/lib64/libart-compiler.so` | art libs | 13.9 | 3.1 | 0.39 | 0 | 13.48 |
| `/system/fonts/NotoSansCJK-Regular.ttc` | fonts | 9.7 | 1.9 | 0 | 0 | 9.69 |
| `/system/lib64/libandroid_runtime.so` | other .so | 8.7 | 2.3 | 0.66 | 0.01 | 8.04 |
| `/apex/com.android.i18n/javalib/core-icu4j.jar` | framework jar | 8.3 | 1.7 | 0 | 0 | 8.32 |
| `/apex/com.android.i18n/etc/icu/icudt72l.dat` | icu | 3.7 | 0.5 | 0 | 0 | 3.73 |
| `[anon:dalvik-/system/framework/boot-framework.art]` | boot image | 12.8/sess | 12.8/sess | **12.8/sess** | 0 | 0 |

* **Shared page cache across sessions:** all `.so` files, framework jars/dex,
  fonts, `framework-res.apk`, `icudt72l.dat`, the file-backed `boot*.oat`/
  `boot*.vdex` (14.1 MiB/session of the boot group), and the `acos-art` binary
  text (shared across the N launchers, 4.8 MiB/session).
* **Privately dirtied copies (not shared at all):** the ART boot images
  `[anon:dalvik-/system/framework/boot-*.art]` = **26.34 MiB/session,
  100 % private-dirty** (answer to "are boot*.art relocated/private-dirty?":
  the *file* maps are clean and shared, but the image data ART builds from them
  is an anonymous private copy per runtime); the region space (39.0), scudo
  (5.8), dalvik control structures (2.4), the launcher's 6.8, the 18.2 MiB of
  gralloc buffers, and the daemon's ~10/session.
* No app `dex2oat` output is mapped (`.art/.oat/.vdex` outside `boot*` ≈ 0);
  the JIT code cache is a 32 MiB `[anon_shmem:dalvik-jit-code-cache]`
  reservation with ~0.02 MiB resident per session.

---

## 4. Marginal per session, and reconciliation with M2's 107.5 MiB

`(N=5 − N=1)/4` and `(N=10 − N=5)/5` per group, from the two-rep means
(MiB/session):

| group | (N5−N1)/4 Pss | (N5−N1)/4 Rss | (N5−N1)/4 PD | (N10−N5)/5 Pss | (N10−N5)/5 Rss |
|---|---|---|---|---|---|
| dalvik heap: main/region space | 38.63 | 38.63 | 38.63 | 39.04 | 39.04 |
| boot image (boot*.art/.oat/.vdex) | 26.51 | 40.36 | 26.34 | 26.33 | 40.44 |
| acos-art host/compositor (anon) | 6.82 | 11.57 | 6.77 | 7.00 | 11.79 |
| malloc (scudo/libc_malloc/[heap]) | 5.79 | 5.79 | 5.79 | 5.91 | 5.91 |
| other .so | 4.74 | 52.37 | 4.43 | 4.39 | 52.21 |
| dalvik linear alloc + other dalvik anon | 2.36 | 2.36 | 2.36 | 2.40 | 2.40 |
| other | 0.99 | 2.04 | 0.93 | 0.96 | 2.01 |
| art runtime libs | 0.34 | 10.85 | 0.35 | 0.33 | 10.80 |
| stacks | 0.30 | 0.35 | 0.28 | 0.69 | 0.74 |
| fonts | 0.20 | 15.49 | 0.00 | −0.04 | 15.44 |
| framework jar/dex | 0.12 | 36.09 | 0.00 | −0.00 | 36.03 |
| memfd/ashmem, LOS, non-moving, jit, icu, resources | ≤0.07 each | | | | |
| **Σ groups** | **86.98** | 223.16 | **86.02** | **87.15** | 224.05 |
| gralloc memfd buffers (cgroup shmem, unmapped) | +18.28 | | | +18.28 | |
| daemon retained anon growth (not a session process) | +9.99 | | | +10.18 | |
| **cgroup anon+shmem marginal** | **113.74** | | | **114.64** | |

The file-backed clean groups have a **near-zero marginal Pss** (fonts 0.20,
framework 0.12) while their Rss marginal is 15–36 MiB: adding a session re-maps
them, it does not pay for them again. The boot image is the opposite: 26.34 of
its 40.4 MiB/session is new private memory every time.

(Σ groups 86.98 + 18.28 + 9.99 = 115.25 vs the cgroup marginal 113.74: the
1.5 MiB difference is group Pss including shared file pages that the cgroup does
not charge as anon/shmem — launcher text, `.so` data — plus smaps Pss rounding.)

### Where the ~113 MiB goes (cgroup decomposition, rep means)

| component | MiB/session | share |
|---|---|---|
| guest anon (region space 39.0, boot image copy 26.3, malloc 5.8, dalvik control 2.4, libs/stacks/other ~4.0) | **78.25** | 68.8 % |
| guest **gralloc memfd buffers** (2 × 9.101, charged to the guest scope's `shmem`, not mapped at settle) | **18.28** | 16.1 % |
| `acos-art` launcher (C1 client, host anon) | **7.27** | 6.4 % |
| `acos serve` daemon retained anon growth (+2 threads/session) | **9.99** | 8.8 % |
| **total** | **113.74** | 100 % |

`(N=10 − N=5)/5` in the same decomposition: guest anon 78.74 + shmem 18.28 +
launcher 7.47 + daemon 10.18 = **114.67** (cgroup 114.64).

### Reconciliation with W57's M2 = 107.50 MiB/session (marginal 107.67)

* Our **N=1 total 117.555 MiB reproduces W57's M1 117.50 MiB**, and our
  daemon-only 1.46 MiB reproduces its 1.355 MiB. The baseline agrees.
* Our incremental is **114.70 / 113.72** MiB/session (two reps; mean 114.21) and
  the marginal 113.74. W57's M2 p50 was 107.50 with a 7.0 % spread and a
  bootstrap 95 % CI of **105.72..113.61**. Our two reps are +0.1 and +1.1 MiB
  above that CI's upper edge (+5.8 % and +6.7 % vs the p50) — the same metric
  type, state, settle and formula, so it is a real (small) difference, not a
  definitional one.
* The arithmetic closes on the one component W57 could not attribute: our
  per-session process tree (launcher+guest) is ~86.4 MiB anon+shmem, the
  buffers 18.28, and the daemon's retained growth 9.99 → 114.7. W57's published
  decomposition was "~88 MiB of each session is the guest process tree"; adding
  the same buffers gives 106.2, leaving **~1.3 MiB/session for the daemon** in
  W57's build. Ours is **~10 MiB/session**. That is the entire gap (and a bit
  more, offset by session-level variance): roughly **8.5 MiB/session more
  daemon retention at `30a693d` than at W57's `bab06c6`**. Two independent
  candidates cannot be separated without W57's smaps: the daemon retention
  grew, and/or the region space ran ~2 MiB/session larger here.
* **The number to carry forward: ~113–115 MiB/session** at N=5/N=10 on
  `30a693d`, with a measured spread of 0.9 % between our N=5 reps and 0.5 %
  between our N=10 reps. 107.5 remains the published W57 row; it is ~5–6 MiB
  below this build, and the difference has a measured address (daemon
  retention), not a definitional one.

---

## 5. Buffers: 2 memfds, 18.2 MiB, per session

Measured from the fd tables and the guest-scope `memory.stat`:

| item | value |
|---|---|
| memfd name | `memfd:acos-gralloc (deleted)` (`memfd_create("acos-gralloc")` + `ftruncate` in the gralloc shim) |
| unique buffers per session | **2** (one per inode; same at N=1, 5, 10) |
| size each | **9.101 MiB** (9,542,880 B = 1080×2209×4; 2400-px screen minus 191-px system bars → 2209-px app surface, confirmed by `live-session.json`) |
| total per session | **18.202 MiB** |
| fds per session | 7 (2 in the guest; 5 in the launcher — imports/dups of the same 2 inodes) |
| who allocates | the **guest**, in the gralloc shim, per `GraphicBuffer`; the launcher imports the same fds over the launch socket (`importBuffer` dups) |
| mapped at settle | **0 mappings** — `lock()` mmaps, `unlock()` munmaps; the pages stay in the tmpfs memfd |
| where charged | guest-scope cgroup **`shmem` = 18.277 MiB/session** (18.202 buffers + ~0.075 JIT cache/binder), *not* in any process's `RssShmem` |
| double/triple? | **double buffered** (2 buffers); the C1 path keeps a third, frame-sized copy in the daemon (§2/§8) |
| share of per-session cost | **18.28 / 113.74 = 16.1 %** (17.0 % of W57's 107.5) |

The M2 cgroup number has always included these 18.2 MiB, but a per-process
`RssAnon+RssShmem` attribution sees 0 of it, which is why W57's per-session
process total (~84–88 MiB) is ~28 MiB below the cgroup marginal.

The **third copy** is in the daemon: unnamed anon grows ~10 MiB/session ≈ one
1080×2209×4 frame (9.10 MiB) plus allocator slack, and it gains exactly
2 threads/session (4→12→22). The measured shape is "one retained frame-sized
anonymous allocation per held session in `acos serve`"; naming the allocation is
D-M2b's job. Cost: 8.8 % of the per-session total — a shared core should not
keep a full frame copy per session.

---

## 6. Java heap

Heap configuration in effect, read from the properties file the launcher wrote
and bound into the sandbox (`evidence/D-M2/n5-rep0/run.json` `guest_props` +
`live_session`; stock API-34 userdebug table in `crates/acos-art/src/props.rs`,
no `ACOS_ART_HEAP*` override in these runs):

| property | value |
|---|---|
| `dalvik.vm.heapstartsize` (`-Xms`) | **8m** |
| `dalvik.vm.heapgrowthlimit` (`-XX:HeapGrowthLimit=`) | **192m** |
| `dalvik.vm.heapsize` (`-Xmx`) | **512m** |
| `dalvik.vm.heapminfree` / `heapmaxfree` | 512k / 8m |
| `dalvik.vm.heaptargetutilization` | 0.75 |
| GC | **Concurrent Copying (CC)** — `[anon:dalvik-region space live bitmap]`, `[anon:dalvik-rb copying gc mark stack]`, `[anon:dalvik-concurrent copying gc mark stack]`, `[anon:dalvik-concurrent copying sweep array free buffer]` are all present |

Measured reservation vs residence, one session (N=5 exemplar, MiB):

| space | reserved (VSS) | Rss | Private_Dirty |
|---|---|---|---|
| `[anon:dalvik-main space (region space)]` | **512.00** | **37.05** (N=1: 40.23; N=5 mean 38.95; N=10 mean 39.00) | all of the Rss |
| `[anon:dalvik-non moving space]` | 64.00 | 0.02 | 0.02 |
| `[anon_shmem:dalvik-jit-code-cache]` | 32.00 | 0.02 | 0.00 |
| `[anon:dalvik-large object space]` + large live/marked | ~0.1 | 0.01–0.13 | same |
| `[anon:dalvik-zygote space]` / `allocspace zygote` bitmaps | 1.0 (bitmap) | **0.00** | 0.00 |
| **total dalvik-managed** | **~609** | **~37–41** | **~37–41** |

Allocated vs free: the region space is a **512 MiB reservation of which ~39 MiB
(7.6 %) is resident**; CC GC returns dead regions with `madvise`, so free heap is
unresident (≈473 MiB free/uncommitted). The heap is not the memory problem — its
reservation is address space, not RSS.

**Would a zygote share this?** The heap *as measured today* is not preloaded
state: the zygote space is empty and the region space is 100 % unique per session
(N=10 unique = 10 × 39.00, sharing = 0.00). The **boot-class state a zygote would
share is the boot image**: 26.34 MiB/session of anonymous private-dirty copy
today (`[anon:dalvik-boot-*.art]`), plus 14 MiB of file-backed
`boot*.oat`/`vdex` that already is shared page cache. A Zygote-style fork of the
post-boot runtime would make the 26.34 MiB copy copy-on-write (only pages each
app writes are duplicated) — the largest clearly-shareable line item in the
113 MiB, ~23 % of the per-session cost — before counting the empty-zygote-space
allocations a real preload would move from per-session region space into the
shared parent.

---

## 7. Evidence and reproduction

```text
results/art-runtime/evidence/D-M2/
  analysis.md, analysis.json.gz          derived tables + every per-process group row
  n1-rep0/   run.json  samples.json.gz  raw/pid-*/{smaps.gz,maps,smaps_rollup,status,
  n5-rep0/                                          cmdline,cgroup,namespaces}
  n5-rep1/   live-session-heap.json (n5-rep0)
  n10-rep0/
  n10-rep1/
```

`run.json` per run holds: git HEAD, binary/APK sha256, host before/after, every
cgroup with full `memory.stat`, the 5 s settled sampler window, every process
record (full argv, fds with memfd inode+size, namespaces, cgroup, rollup), the
drawn trees, pid→session map, the guest property table, and the
survivor/leftover scan. `raw/pid-*/cmdline` is the **full, unredacted argv** of
every process of every run (the bwrap argv is ~8 KiB ×2 per session, identical
in both bwrap processes except the per-launch run directories; that is why it is
in the raw files rather than pasted here).

Reproduce (from a clean host, host-wide ART lock):

```sh
CARGO_TARGET_DIR=.cache/dm2-target cargo build --release -p acos-cli -p acos-art
for n in 1 5 10; do
  tools/art-runtime/run-exclusive.sh --task DM2a -- \
    tools/guarded.sh --mem 12G --time 1200 -- \
    python3 -I tools/art-runtime/dm2/measure.py --phase run \
      --acos .cache/dm2-target/release/acos --n $n --rep 0 --settle 60 \
      --outdir .cache/dm2/runs
done
python3 tools/art-runtime/dm2/measure.py --phase analyze \
  --dir results/art-runtime/evidence/D-M2
```

`measure.py --phase self-test` (17 classifier checks, no ART) and
`tools/tests/test-d2-warm.sh` (**15/15**) both pass; `d2_warm.py` and
`bench-d2.sh` were not modified.

## 8. Surprises, in order of consequence

1. **18.2 MiB/session of graphics buffers are charged to the cgroup but mapped
   by no process at settle.** Two 9.101 MiB `acos-gralloc` memfds per session,
   double-buffered, guest-allocated, launcher-imported, `munmap`ed after each
   frame and kept alive in tmpfs. 16.1 % of the per-session cost, invisible to
   any per-process RSS attribution.
2. **The shared daemon retains ~10 MiB and 2 threads per held session.** The
   "lightweight shared core" is not flat: `acos serve`'s unnamed anon grows
   10.3 → 50 → 101 MiB at N=1/5/10, ≈ one frame-sized allocation per session.
   It also explains essentially all of the gap between our 113.7 MiB and W57's
   107.5 MiB (§4).
3. **26.34 MiB/session of boot image is a private anonymous copy**
   (`[anon:dalvik-boot-*.art]`, 100 % Private_Dirty), while the file-backed
   `boot*.oat`/`.vdex` maps (14 MiB/session) are shared page cache. A fork of a
   booted runtime would COW the 26.34 MiB.
4. **`Private_Clean` is not private.** The same read-only jar/oat pages flip
   from Private_Clean (N=1) to Shared_Clean (N=5) as more processes map them,
   and Pss drops to 1/N. Adding Private_Clean to the private cost overstates it
   by ~120 MiB/session at N=1.
5. **The N=1 tree has no zygote space at all** (`dalvik-zygote space` 0.00,
   `allocspace zygote` bitmaps empty): every session pays the full ART boot
   (boot-image copy, framework jar/oat parsing, region-space boot allocations).
   The fixed part is small (daemon + launcher + one page-cache set) and the
   marginal part is almost entirely anonymous private memory plus buffers, which
   is why N=5 costs 4.87× N=1 rather than 5×.
