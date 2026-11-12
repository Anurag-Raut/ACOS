# Z6 — the owner's 25 % gate, measured on today's `main` (W1 + P9/P10), at three time points

**Gate under test (verbatim).** *"Only recommend production integration if the new architecture
demonstrates at least a 25 % whole-system improvement over the optimised current implementation,
without sacrificing correctness, security or compatibility."*

This file is measurement only: the numbers, the advantages, the disadvantages and the risks, with
every threshold verdict stated as a fact. It does **not** make the recommendation.

Worktree `zygote-z6-final` (branch `zygote-z6-final`; `main` + Z4 + B3 + Z5 + **W1** + **P9/P10**).
One binary for all arms (`CARGO_TARGET_DIR=.cache/z6-target cargo build --release -p acos-cli
-p acos-art`), one harness (`research/zygote-proto/z4/drive.py`, Z6 additions only), one fixture
(`fixtures/build/apps/counter.apk`). Every ART run went through
`tools/art-runtime/run-exclusive.sh --task Z6-* -- tools/guarded.sh --mem 24G --time <n> -- …`.

---

## 0. Facts up front

| # | fact |
|---|---|
| 1 | **The launcher fix (W1) moved the denominator, not the numerator.** The launcher is now **3.6 MiB** Pss per session at 0.5 s (**4.5 MiB** at 60 s) instead of Z5's 20.5–25.2 MiB. Every arm got cheaper, so every *percentage* in the gate table below is different from Z5's even where the guests are unchanged: arm (a) at N=5 is **517.1 MiB** (0.5 s), Z5 published **624.5 MiB**. |
| 2 | All three arms, all N, **pass every correctness check**: `0` → real InputChannel tap → `1`, the ordered isolation matrix (0/10/45/190 checks at N=1/5/10/20), byte-identical C1 pixels (`39d1e4e3…` initial, `9f3406c1…` after the tap — the same md5s as Z3/Z4/Z5), and the post-settle repeat (`1` → tap → `2` in-session, others unchanged). |
| 3 | **Z5's open question is answered.** A zygote child releases **43.6 MiB in one second, 9.2–10.2 s after its first frame, and the released pages are entirely `[anon:dalvik-main space (region space)]`** (49.0 → 4.2 MiB Pss; every other mapping unchanged). The schedule is ART's post-fork footprint shrink (`ReduceTargetFootprintTask` at +2 s and +10 s to `dalvik.vm.heapstartsize`, `Heap::PostForkChildAction`), which the exec'd path never runs; the exec'd guest does collect on its own ~14 s after first frame and keeps 90 % of its region space (36.3 → 32.6 MiB), so the difference is **structural, not a GC-timing artefact** (§5). |
| 4 | **Gate verdicts** (whole system ΣPss, same N, same time point): (c) is **+63.1/+64.0/+59.6 % at N=10** and **+67.1/+66.1/+64.3 % at N=20** versus (a) at 0.5/15/60 s; **+46.5/+45.8 % at N=5 from 15 s** but only **+6.7 % at N=5 at 0.5 s**; **−7.6 % at N=1 at 0.5 s** and +4.7/+6.9 % at 15/60 s. Versus (b) the same pattern, 2–7 points lower at N ≥ 5. Arm (b) alone never reaches 25 % (§2). |
| 5 | **Swap is zero everywhere**: `ΣSwapPss = 0.000 MiB` in every configuration and every process, `memory.swap.max=0` and `memory.swap.current=0` on every scope sampled (`/sys/fs/cgroup/…`, §9). |
| 6 | **Security is unchanged from ZS.** Z6 changed no sandbox line: untrusted APKs stay on the exec path; F1/F2/F3/F4–F7/F8/F10 and the (b)+(c) incompatibility (F11) stand exactly as ZS recorded them (re-confirmed by the Z4SEC probes in these runs, §9). |
| 7 | Arms (b) and (c) still cannot be stacked (ZS F11), so **no measured configuration contains both optimisations**. |

---

## 1. Method

### 1.1 Arms (all on the same binary, release)

| arm | path | environment |
|---|---|---|
| **(a) optimised current** | `main` default: one `acos-art launch` per session, one exec'd ART guest each | no `ACOS_*` seam set |
| **(b) (a) + B3 shared boot image** | same, uncompressed image at a fixed base (`-Ximage:/data/img/boot.art -Xnorelocate`) | `ACOS_SHARED_BOOT_IMAGE=.cache/b2/image` |
| **(c) zygote (Z4 seam) + W1** | one pre-initialised ART zygote forks one child per session; each child enters its own mount view and the unmodified `LaunchMain.main` | `ACOS_ZYGOTE_SESSION/PLAN_DIR/N`, `ACOS_ZYGOTE_NO_STUB=1` (Z5's 0.14 MiB host-`sleep` holder instead of the 9.1 MiB tombstone) |
| **(d) control, not a gate arm** | (a) with a tighter managed-heap policy — the existing W48 knobs, no code change. §5.6: the knob does **not** reach this guest, so (d) measured the same guest as (a) | `ACOS_ART_HEAPGROWTHLIMIT=48m|24m`, `ACOS_ART_HEAPTARGETUTILIZATION=0.9` |

(a) is the production path: with every `ACOS_ZYGOTE_*` variable unset the launcher's argv,
class path, guest dex, bind list, seccomp preload, run dir and socket dir are production's.

### 1.2 One run, three time points

Per configuration: N sessions of the counter fixture, each a real `acos-art launch --apk … --until
activity-resumed --interactive` with its own ServiceHost/InputHost/C1/sockets/run root. The driver
waits for each session's own `ready` line, reads the tree (`:id/count` = `0`), taps `:id/inc` with
a real InputChannel tap, re-reads the count on a new settled frame (`1`), re-reads every
not-yet-tapped session (`0`), and screenshots; then, with all N sessions held, it samples
`/proc/<pid>/smaps_rollup` for every process in its tree at **0.5 s, 15 s and 60 s** after the
last tap — all three from the same sessions, on the same host minute — and finally proves after
the 60 s sample that every session still taps to `2` and stays isolated.

The brief's "first frame (0.5 s)" is Z5/Z4's method: the clock starts when the last tap's reply
has been read. That is **3 s after spawn at N=1 and 65 s after spawn at N=20**, because the tap +
isolation phase grows with N — measured per configuration in §5.4 (the driver also records every
session's own `t_ready_ms`, so the offset from each first frame is in the evidence).

### 1.3 Definitions

* **whole system** = every process in the driver's tree **except the driver**: N launchers, N
  bwrap sandboxes (2 per session in (a)/(b)), the guests (or the zygote + its forked children +
  its `z4ns`/`z4init` helpers) and the (c) holders. Same definition as Z5.
* **ΣPss** = Σ`Pss` from `smaps_rollup` (shared pages apportioned); **ΣPD** = Σ`Private_Dirty`.
* **marginal** = least-squares slope of ΣPss over N = 1/5/10/20, MiB per extra session; the fit's
  intercept is the **fixed shared cost**.
* **saving** = (a − x)/a at the same N and the same time point; positive = x uses less memory.
* **t** = seconds after the last tap's acknowledgement (0.5 = Z4/Z5's method, 60 = D-M2's steady
  state; 15 s is new here, and it is the first point at which every session has had ≥15 s of idle
  time after its own first frame).

### 1.4 Harness changes (Z6 only, no product change)

`research/zygote-proto/z4/drive.py`:

1. `--samples 0.5,15,60` — the three whole-tree snapshots in one run (cumulative holds). Each
   snapshot also records per-role totals, `Swap`/`SwapPss` per process, and the cgroup files of
   every scope it sees (`memory.swap.max`, `memory.swap.current`, `memory.max`,
   `memory.current`, `memory.oom.group`, `pids.max/current`, `memory.events`).
2. `--release-probe SECS` — 1 Hz sampling for `SECS` seconds **before** the tap sequence (so the
   sessions are idle): `smaps_rollup` for every managed process, full `/proc/<pid>/smaps`
   attribution for every app child, the zygote parent and one launcher
   (`research/zygote-proto/z6/smaps.py`), and the guest log sink (`{"cmd":"logs"}`, i.e. what
   `logcat` would show) filtered for GC/trim/Zygote lines and tagged with the second it appeared
   (`research/zygote-proto/z6/probe.py`).
3. The two `comm`-derived sub-strings of the Z4 harness were made truncation-proof: `/proc/comm`
   drops the leading byte of `fixtures.counter` (16 bytes, no terminator), so the child is
   `ixtures.counter` (ZM1 §1.4 hit the same). Nothing else in the Z4/Z5 protocol changed.

No product code was touched: (d) uses the existing `ACOS_ART_HEAP*` seam (W48).

---

## 2. The gate table

Whole-system ΣPss, MiB. Arm (a) = optimised current (`main` default, W1 in), (b) = (a) + B3's
shared boot image, (c) = zygote + `ACOS_ZYGOTE_NO_STUB=1`. Same binary, same fixture, same host;
`t` = seconds after the last tap's acknowledgement. Every cell is one configuration of one driver
run, and the three time points of a row come from that same run. Note that `t = 0.5 s` is **3–65 s
after the sessions' first frames** depending on N (the tap + isolation phase grows with N); this is
measured and tabulated in §5.4, and it is why the 15 s/60 s columns are the comparable ones.

### ΣPss and ΣPrivate_Dirty per arm, N and time point

| arm | N | 0.5 s | ΣPD 0.5 s | 15 s | ΣPD 15 s | 60 s | ΣPD 60 s | verdict |
|---|---|---:|---:|---:|---:|---:|---:|---|
| (a) | 1 | 209.1 | 79.0 | 210.4 | 80.3 | 209.2 | 78.8 | pass |
| (a) | 5 | 517.1 | 381.5 | 517.5 | 381.7 | 541.8 | 405.9 | pass |
| (a) | 10 | 903.4 | 765.7 | 951.1 | 813.4 | 952.2 | 814.5 | pass |
| (a) | 20 | 1723.7 | 1583.8 | 1703.3 | 1563.3 | 1748.9 | 1608.7 | pass |
| (b) | 1 | 241.0 | 66.5 | 240.9 | 66.0 | 238.5 | 63.4 | pass |
| (b) | 5 | 502.8 | 322.0 | 505.2 | 324.4 | 527.5 | 346.4 | pass |
| (b) | 10 | 816.1 | 633.4 | 862.6 | 679.6 | 862.6 | 679.6 | pass |
| (b) | 20 | 1512.8 | 1327.0 | 1488.9 | 1303.1 | 1487.6 | 1301.8 | pass |
| **(c)** | **1** | **224.9** | 48.9 | **195.9** | 19.9 | **199.4** | 23.4 | pass |
| **(c)** | **5** | **482.7** | 304.0 | **276.9** | 98.2 | **293.8** | 115.1 | pass |
| **(c)** | **10** | **333.4** | 152.8 | **342.6** | 162.0 | **384.8** | 204.2 | pass |
| **(c)** | **20** | **567.1** | 378.8 | **577.0** | 388.8 | **624.2** | 436.0 | pass |

(The (c) N=2 smoke run — 215.9 MiB at 0.5 s, 5 s only — is in the evidence but not in the table.)

### Saving at the same N (positive = the arm uses less)

| N | t | (a) | (b) | (c) | **(c) vs (a)** | **(c) vs (b)** | (b) vs (a) |
|---:|---|---:|---:|---:|---:|---:|---:|
| 1 | 0.5 s | 209.1 | 241.0 | 224.9 | **−7.6 %** | +6.7 % | −15.3 % |
| 1 | 15 s | 210.4 | 240.9 | 195.9 | **+6.9 %** | +18.7 % | −14.5 % |
| 1 | 60 s | 209.2 | 238.5 | 199.4 | **+4.7 %** | +16.4 % | −14.0 % |
| 5 | 0.5 s | 517.1 | 502.8 | 482.7 | **+6.7 %** | +4.0 % | +2.8 % |
| 5 | 15 s | 517.5 | 505.2 | 276.9 | **+46.5 %** | +45.2 % | +2.4 % |
| 5 | 60 s | 541.8 | 527.5 | 293.8 | **+45.8 %** | +44.3 % | +2.6 % |
| 10 | 0.5 s | 903.4 | 816.1 | 333.4 | **+63.1 %** | +59.1 % | +9.7 % |
| 10 | 15 s | 951.1 | 862.6 | 342.6 | **+64.0 %** | +60.3 % | +9.3 % |
| 10 | 60 s | 952.2 | 862.6 | 384.8 | **+59.6 %** | +55.4 % | +9.4 % |
| 20 | 0.5 s | 1723.7 | 1512.8 | 567.1 | **+67.1 %** | +62.5 % | +12.2 % |
| 20 | 15 s | 1703.3 | 1488.9 | 577.0 | **+66.1 %** | +61.2 % | +12.6 % |
| 20 | 60 s | 1748.9 | 1487.6 | 624.2 | **+64.3 %** | +58.0 % | +14.9 % |

### Where the 25 % threshold holds (plain statement)

* **(c) vs the optimised current path (a):** met at **N = 10 and N = 20 at all three time points**
  (59.6–67.1 %), and at **N = 5 from 15 s onwards** (46.5 % / 45.8 %). **Not met at N = 5 at 0.5 s**
  (+6.7 %) and **not met at N = 1** (−7.6 % at 0.5 s, +4.7 % to +6.9 % at 15/60 s).
* **(c) vs (a)+(b) — the current path including B3's shared image:** the same pattern, 2–7 points
  lower at N ≥ 5: met at N = 10 (55.4–60.3 %), N = 20 (58.0–62.5 %) and N = 5 from 15 s onwards
  (44.3–45.2 %); not met at N = 5 at 0.5 s (+4.0 %) nor at N = 1 (+6.7 % to +18.7 % — (b) is the
  worse baseline at N = 1).
* **(b) alone never reaches 25 %** against (a): +9.3 % to +14.9 % at N ≥ 10, +2.4 % to +2.8 % at
  N = 5, −14.0 % to −15.3 % at N = 1. So the "optimised current implementation" the gate names is
  (b) at N ≥ 5, and the (c)-vs-(b) column above is the strictest reading of the gate.
* The single-session row is the counter-example in every arm: at N = 1 the zygote pays the
  pre-fork core (~98 MiB, §3) for one child, (b) pays its image per process, and only the steady
  state of (c) is (slightly) ahead.

---

## 3. Marginals and the fixed shared cost

Least squares over N = 1/5/10/20, MiB per extra session. "Fixed cost" is the fit's intercept:
for (c) it is physically the pre-fork core (zygote parent + its two research helpers), for (a)/(b)
it is the extrapolated offset of the fit (a single session already costs ~209 MiB, so a linear fit
through N ≥ 1 separates a per-session part from a constant part that has no separate process).

| arm | t | marginal | fixed cost | guest side / session | child Private_Dirty / session |
|---|---|---|---:|---:|---:|
| (a) | 0.5 s | 79.8 | 120.0 | 76.0 | 75.8 |
| (a) | 15 s | 78.9 | 135.7 | 74.9 | 74.6 |
| (a) | 60 s | 80.9 | 134.6 | 76.2 | 75.9 |
| (b) | 0.5 s | 66.9 | 165.9 | 63.1 | 62.8 |
| (b) | 15 s | 65.8 | 182.2 | 61.8 | 61.5 |
| (b) | 60 s | 65.4 | 190.5 | 60.7 | 60.3 |
| **(c)** | 0.5 s | **16.1** | 242.5 | 9.4 | 8.0 |
| **(c)** | 15 s | **19.9** | 169.3 | 11.8 | 10.4 |
| **(c)** | 60 s | **22.2** | 175.9 | 12.2 | 10.8 |

* **Marginal whole-system cost of one more session: 80.9 (a) / 65.4 (b) / 22.2 (c) MiB** at 60 s —
  (c) is **72.6 % below (a)** and **66.1 % below (b)**; at 15 s the numbers are 78.9 / 65.8 / 19.9.
  The 0.5 s (c) slope (16.1) must not be quoted: at 0.5 s the (c) series is not monotonic in N
  (224.9 / 482.7 / 333.4 / 567.1 at N = 1/5/10/20) because the N=5 run is the only one whose
  children are sampled *before* they release (§5), so a line fitted through it is not a
  steady-state marginal.
* **Fixed shared cost of (c), measured directly at N=1** (60 s sample): zygote parent **77.8 MiB**
  Pss (Private_Dirty 0.6, Shared_Dirty 36.1), `z4ns` **10.4**, `z4init` **9.6**, bwrap 0.2 —
  **98.0 MiB before the first app child**. The two helpers (20.0 MiB) are research scaffolding a
  product zygote would not spawn (Z4 §6.1); the app child itself is 89.2 MiB at N=1 because it is
  the only mapper sharing the image.
* (c)'s per-session holder is **0.14 MiB** (`/bin/sleep`, Z5's `ACOS_ZYGOTE_NO_STUB`; measured
  0.6 / 1.2 / 2.5 MiB at N = 5 / 10 / 20), not Z4's 9.1 MiB tombstone.
* (b)'s marginal (65–67 MiB) is (a)'s minus the per-process image copy: 26.3 MiB of private image
  per session becomes one shared page-cache copy. Its *fixed* cost is higher than (a)'s because at
  small N the image is apportioned over fewer processes; that is why (b) is 14–15 % **worse** than
  (a) at N=1 and 12–15 % better at N=20.

---

## 4. Where the memory is

Per-session app child (mean over the session's children), Pss / Private_Dirty, MiB:

| cell | 0.5 s | 15 s | 60 s |
|---|---:|---:|---:|
| (a) N=1 | 201 / 77 | 202 / 78 | 201 / 75 |
| (a) N=5 | 100 / 74 | 99 / 74 | 104 / 78 |
| (a) N=10 | 87 / 74 | 91 / 78 | 91 / 78 |
| (a) N=20 | 82 / 76 | 81 / 74 | 83 / 76 |
| (b) N=5 | 97 / 62 | 97 / 62 | 101 / 66 |
| (b) N=20 | 72 / 63 | 70 / 61 | 70 / 61 |
| **(c) N=1** | 119 / 46 | **89 / 16** | 89 / 16 |
| **(c) N=5** | 77 / 58 | **35 / 15** | 35 / 15 |
| **(c) N=10** | 21 / 11 | 21 / 11 | 22 / 12 |
| **(c) N=20** | 17 / 12 | 16 / 11 | 17 / 11 |

Composition at 60 s, MiB Pss ("zygote stack" = zygote parent + `z4ns`/`z4init` + bwrap):

| arm | N | app children | zygote stack | launchers | bwrap | holders | total |
|---|---|---:|---:|---:|---:|---:|---:|
| (a) | 1 | 200.6 | 0.0 | 8.4 | 0.3 | 0.0 | 209.2 |
| (a) | 5 | 517.9 | 0.0 | 22.7 | 1.2 | 0.0 | 541.8 |
| (a) | 10 | 907.3 | 0.0 | 42.5 | 2.3 | 0.0 | 952.2 |
| (a) | 20 | 1651.5 | 0.0 | 92.9 | 4.5 | 0.0 | 1748.9 |
| (b) | 1 | 229.9 | 0.0 | 8.4 | 0.3 | 0.0 | 238.5 |
| (b) | 5 | 503.6 | 0.0 | 22.7 | 1.2 | 0.0 | 527.5 |
| (b) | 10 | 817.7 | 0.0 | 42.5 | 2.3 | 0.0 | 862.6 |
| (b) | 20 | 1390.2 | 0.0 | 92.9 | 4.5 | 0.0 | 1487.6 |
| (c) | 1 | 89.2 | 98.1 | 12.1 | 0.0 | 0.0 | 199.4 |
| (c) | 5 | 172.8 | 77.1 | 43.3 | 0.0 | 0.6 | 293.8 |
| (c) | 10 | 217.9 | 73.6 | 92.2 | 0.0 | 1.2 | 384.8 |
| (c) | 20 | 331.1 | 77.6 | 212.9 | 0.0 | 2.5 | 624.2 |

Facts that follow from these two tables:

1. **The whole difference is on the guest side.** At N=20 and 60 s: children 1651.5 (a) / 1390.2 (b)
   / 331.1 (c); launchers 92.9 / 92.9 / 212.9; everything else ≤ 5 MiB except (c)'s 77.6 MiB zygote
   stack.
2. **A (c) child's Private_Dirty is 11–16 MiB**, an (a) child's is 74–78 MiB. The exec'd guest's
   26.3 MiB private image copy and its 32–36 MiB private region space are replaced by one shared
   image and a 4–7 MiB private heap (§5); what remains on the child's own account is its live app
   data.
3. **A (c) child's Pss falls as N grows** (89 → 35 → 21 → 17) while an (a) child's only falls from
   201 to 82: the zygote's image + pre-fork heap (36.1 MiB of `Shared_Dirty` per child) is one
   physical copy apportioned over every mapper, so its per-child *Pss* shrinks with N and its
   *private* cost does not exist at all.
4. **Arms (a) and (b) grow while idle; (c) does not.** Between the 0.5 s and 60 s samples: (a)
   +24.7 / +48.8 / +25.2 MiB at N = 5 / 10 / 20, (b) +24.7 / +46.5 / −25.2 MiB, (c) −188.9 /
   +51.4 / +57.1 MiB — and (c)'s *rise* is entirely its launchers (N=20: launchers +64.4 MiB while
   the zygote stack + children fall 7.2 MiB). (a)/(b) rise because the
   exec'd guest keeps its launch garbage (§5) and keeps allocating while idle (region space,
   malloc, gralloc: ZM1's +7 MiB/child over a session); the launchers add ~+1 MiB each.
5. **(c)'s launchers cost more than (a)'s: 10.6 vs 4.6 MiB at N=20** (+4.1 to +6.0 MiB per
   session). In (c) the launcher grows from 4.0 MiB at 0.5 s to 8.6–10.6 MiB at 60 s; in (a) from
   3.6 to 4.5–4.6. This is a property of the Z4 foreign-guest seam (a second `acos-art` process per
   session that holds the session and pumps the child's stdout), not of the fork, and it is the
   single largest non-guest cost in (c): at N=20 it is 212.9 of 624.2 MiB (34 %).
6. **The per-session ABI of (c)'s win is the shared pre-fork core**, exactly as Z4 §3.3/§4 and
   ZM1 §2 measured it: boot image + framework classes + JNI table live once (77.8 MiB Pss in the
   zygote parent, 36.1 MiB `Shared_Dirty` mapped by every child) instead of being decompressed per
   process ((a): 26.3 MiB private image per session).

### 4.1 Why the (c) launchers are heavier (attribution)

The 20 s probe attributes one launcher per run. In (c) it grows **3.24 → 4.74 MiB in 19 s**, and
the growth is entirely in **unnamed anonymous mappings** (`[anon]` 2.96 → 4.46 MiB): `[heap]` is
flat at 0.25 MiB, the ART/region-space and `dalvik-*` buckets are 0.00, and the binary's text is
0.84 MiB and constant. In arm (a) the same launcher grows 3.6 → 4.6 MiB over 60 s.
So a (c) launcher accumulates ~78 KiB/s of mmap'd anon buffer (≈ +4.6 MiB per launcher by 60 s) that
an (a) launcher does not. The probe names the *mapping class*, not the allocation site: the cause is
in the Z4 foreign-guest seam (the second `acos-art` per session that owns the session and pumps the
child's stdout FIFO) and was not fixed here — it is the largest avoidable non-guest cost in (c)
(212.9 of 624.2 MiB at N=20, §4).

---

## 5. The release mechanism: what drops, when, and would (a) do it too?

Z5 left this open (its §3: "mechanism: not established in the time box"). It is established here by
a 1 Hz measurement, and the answer also explains why Z5's 0.5 s and 60 s protocols disagreed.

### 5.1 The timeline, per child, at 1 Hz for 20 s (arm (c), N=5)

The probe runs **before the tap sequence**, i.e. on idle sessions, starting as soon as the last
session has drawn its first frame. Pss in MiB, `smaps_rollup`, one row per second; the five app
children are shown (they are within 0.4 MiB of each other at every second):

| probe t (s) | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | **9** | 10 | 11 | 12 | 15 | 19 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| child A | 62.8 | 64.8 | 66.8 | 66.8 | 68.9 | 70.9 | 72.9 | 74.9 | 76.9 | **33.3** | 35.4 | 35.3 | 35.0 | 34.9 | 34.8 |
| child B | 63.0 | 65.0 | 67.0 | 67.0 | 69.1 | 71.1 | 73.1 | 75.1 | 77.1 | **33.5** | 35.7 | 35.5 | 35.3 | 35.2 | 35.0 |
| child C | 62.6 | 64.6 | 66.6 | 66.6 | 68.7 | 70.7 | 72.7 | 74.7 | 76.7 | **33.2** | 35.3 | 35.2 | 34.9 | 34.8 | 34.7 |
| child D | 62.6 | 64.6 | 66.6 | 68.6 | 68.6 | 70.6 | 72.6 | 74.6 | 76.6 | **33.2** | 35.3 | 35.2 | 34.9 | 34.8 | 34.7 |
| child E | 62.9 | 65.0 | 67.0 | 69.0 | 69.0 | 71.0 | 73.0 | 75.0 | 77.0 | **33.5** | 35.7 | 35.5 | 35.3 | 35.2 | 35.0 |
| Σ whole tree | 409.0 | 419.5 | 430.0 | 434.4 | 440.8 | 451.1 | 461.6 | 472.0 | 482.4 | **265.3** | 276.4 | 276.0 | 275.2 | 275.8 | 276.7 |

* **The release happens in one 1 s bucket, in all five children at once: −43.6 MiB Pss each
  (56.6 %), −43.5 MiB Private_Dirty.** After it the child settles at 33.3 → 35.4 MiB within one second and then stays flat
  (34.7–35.2 MiB for the remaining 10 s of the window).
* Before it, the child was **growing ~2 MiB/s** (35.0 → 49.0 MiB of region space in 8 s): the app was
  still finishing its launch work after the first frame.
* **When, relative to the first frame:** the probe starts 1.6 s after spawn (all-ready), and the five
  sessions' first frames were at 0.46 / 0.77 / 0.98 / 1.19 / 1.40 s, so the drop at probe t=9 is
  **9.2–10.2 s after each child's own first frame** (≈9.4–10.4 s after its `fork`).

### 5.2 Attribution by mapping: it is the region space, and nothing else

One (c) child, full `/proc/<pid>/smaps` bucketed by mapping name (`research/zygote-proto/z6/smaps.py`):

| probe t | region space | main space | zygote space | image | malloc | other dalvik anon | gralloc | everything else | total | Private_Dirty |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 s | 35.0 | 0.4 | 0.1 | 2.4 | 5.6 | 3.9 | 0.1 | 15.1 | 62.3 | 43.5 |
| 4 s | 41.0 | 0.4 | 0.1 | 2.4 | 5.6 | 3.9 | 0.1 | 15.1 | 68.3 | 49.5 |
| 8 s | **49.0** | 0.4 | 0.1 | 2.4 | 5.6 | 3.9 | 0.1 | 15.1 | 76.3 | 57.5 |
| **9 s** | **4.2** | 0.1 | 0.1 | 2.4 | 5.7 | 3.8 | 0.1 | 15.1 | **32.8** | **14.0** |
| 19 s | 7.0 | 0.1 | 0.1 | 2.4 | 5.7 | 3.8 | 0.1 | 15.1 | 34.3 | 15.5 |

(`region space` and `main space` are the same ART space; the parser keeps them separate because
`[anon:dalvik-main space (region space)]` and `[anon:dalvik-region space live bitmap]` are two
mappings. "Everything else" is file-backed text/page cache.)

The top mappings at the two seconds, verbatim:

```text
t=8  [anon:dalvik-main space (region space)]            pss=49.0 pd=49.0
     [anon:dalvik-/system/framework/boot-framework.art] pss= 1.7 pd= 0.9
     /system/framework/x86_64/boot-framework.oat        pss= 1.4 pd= 0.0
t=9  [anon:dalvik-main space (region space)]            pss= 4.2 pd= 4.2
     [anon:dalvik-/system/framework/boot-framework.art] pss= 1.7 pd= 0.9
     [anon:dalvik-region space live bitmap]             pss= 1.2 pd= 1.2
```

**Nothing but the region space moves**, and it is freed with `madvise` (the mapping survives at the
same size; only its resident pages are returned). The shared pre-fork state
(`boot-framework.art`, zygote space, the file mappings) does not change at all — ZM1's CoW-erosion
result (0.08 MiB/child over a session) is confirmed from the other direction.

### 5.3 The AOSP path that schedules it

1. `ZygoteHooks.postForkChild` → `ZygoteHooks_nativePostForkChild`
   (`.cache/aosp/art/runtime/native/dalvik_system_ZygoteHooks.cc:294`), which calls
   `Runtime::SetAsZygoteChild` and then **`runtime->GetHeap()->PostForkChildAction(thread)`**
   (`:348`).
2. `Heap::PostForkChildAction` (`art/runtime/gc/heap.cc:4692`) sets the target footprint to
   `growth_limit_` (`:4705`, so launch may allocate up to the heap growth limit) and then schedules
   **two `ReduceTargetFootprintTask`s**: at **+2 s** to `max(growth_limit_/4, initial_heap_size_)`
   (`:4710-4716`) and at **+10 s** to `initial_heap_size_` (`:4718-4724`), where the interval is
   `kPostForkMaxHeapDurationMS = 2000` (`:202`) and `initial_heap_size_` is
   `dalvik.vm.heapstartsize` = 8 MiB. It also schedules `TriggerPostForkCCGcTask` at
   +8 s + U(0, 20 s) (`:4726-4733`).
3. `ReduceTargetFootprintTask::Run` only lowers `target_footprint_` (`:4660-4688`); **the next
   allocation over the new target requests the GC**. That is why the drop lands at ~10 s and not at
   the +2 s task: the child was still allocating ~2 MiB/s, crossed the 8 MiB target, and the CC GC
   started.
4. The CC GC's from-space clearing returns the pages: `ConcurrentCopying::…ClearFromSpace` →
   `RegionSpace::ClearFromSpace` (`art/runtime/gc/space/region_space.cc:416`) →
   `ZeroAndProtectRegion`/`ZeroMemory` (`:393-398`, `libartbase/base/mem_map.cc:1285`) and
   `RegionSpace::ReleaseFreeRegions` (`region_space.cc:403-415`, `madvise(MADV_DONTNEED)`), reached
   from `concurrent_copying.cc:2804`.
5. The eager path is gated by `GarbageCollector::ShouldEagerlyReleaseMemoryToOS()`
   (`art/runtime/gc/collector/garbage_collector.cc:355`): true for a zygote, for an explicit GC, or
   when the process is **not** jank-perceptible. The child is not a zygote
   (`SetAsZygoteChild(false,false)`), and the GC is not explicit — so the applicable clause is
   `!InJankPerceptibleProcessState()`. *Inferred, not read out:* on this kernel `madvise(MADV_FREE)`
   returns **EINVAL** (measured: a 64 MiB anon range, touched, `MADV_FREE` → `-1 EINVAL`,
   `MADV_DONTNEED` → 0 and Pss 68 → 4 MiB), so the non-eager branch (`ZeroMemory(..., false)` →
   `ClearMemory(resident=true)` → `madvise(MADV_FREE)`, `mem_map.cc:1274-1281`) would have hit
   ART's `CHECK_NE(res, -1)` and aborted the child. No child aborted; the pages were returned. So the
   eager branch ran.

**The exec'd guest of arms (a)/(b) never runs step 1–2 at all.** A non-zygote process initialises
through `Runtime::Init` → `Runtime::InitNonZygoteOrPostFork` (`art/runtime/runtime.cc:1118`, function
at `:1195`), which schedules no footprint task. That is the whole difference in *policy*.

**The ART log cannot pin the second, and this is why Z5 could not close the question.**
`Heap::PostForkChildAction` does log `Using CollectorTypeCC GC.` (one `LOG(INFO)` per forked child —
six of them appear in the guest sink at the probe's t=0, which is how the fork itself is timestamped),
and the zygote's pre-fork `ZygoteHooks.gcAndFinalize()` logs
`Explicit concurrent copying GC freed 12477(655KB) …`. But `Heap::LogGC`
(`art/runtime/gc/heap.cc:2862`) only emits a `GC freed` line for an **explicit** GC or a **slow** one
(`kLogAllGCs = false`, `heap.cc:198`), and the post-fork background CC GC is neither. **No log line
appears at the release second**; the 1 Hz `smaps_rollup` is what pins it.

### 5.4 Why Z5's two protocols disagreed — the sampling offset grows with N

The driver's "0.5 s" sample is 0.5 s after the **last tap**, and the tap+isolation phase grows with
N. Measured offsets from spawn to the 0.5 s sample:

| configuration | all N ready | first sample at | tap+isolation phase | child state at that sample |
|---|---:|---:|---:|---|
| (c) N=1 | 0.6 s | spawn + 3.0 s | 2.4 s | **pre-release** (89 MiB) |
| (c) N=5 | 1.6 s | spawn + 10.0 s | 8.4 s | **pre-release** (77 MiB) |
| (c) N=10 | 2.8 s | spawn + 24.0 s | 21.2 s | post-release (21 MiB) |
| (c) N=20 | 5.4 s | spawn + 65.0 s | 59.6 s | post-release (17 MiB) |
| (a) N=5 / N=10 / N=20 | 1.4 / 2.5 / 4.8 s | +10.0 / +24.0 / +65.0 s | 8.6 / 21.5 / 60.2 s | unchanged in every case |

So Z5's N=5 row measured (c) *before* its release and its N=10/20 rows measured it *after* — not an
inconsistency of the arm, but one protocol applied to two different settling behaviours. **In Z6 the
15 s and 60 s columns are the comparable ones** (every child has had ≥15 s of idle time after its own
first frame at both), and the 0.5 s column is reported for continuity with Z4/Z5, flagged as
3–65 s after the first frame depending on N.

### 5.5 Would arm (a) release the same memory if ART ran the same trim?

**A GC does run in arm (a), and it returns ~10 % of the region space.** Same 1 Hz probe, same host,
same fixture, arm (a) N=5 (child Pss / its region-space bucket, MiB):

| probe t | 0 | 2 | 4 | 6 | 8 | 10 | 12 | **14** | 15 | 17 | 19 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| child Pss | 97.6 | 99.6 | 99.6 | 99.9 | 101.9 | 101.9 | 102.0 | **98.4** | 99.1 | 99.8 | 99.8 |
| region space | 32.3 | 34.3 | 34.3 | 34.3 | 36.3 | 36.3 | 36.3 | **32.6** | 33.0 | 33.6 | 34.3 |

At t≈14 s a collection happens in the exec'd guest too (region space 36.3 → 32.6, −3.7 MiB) and the
process **keeps 32.6 MiB**. So in (a) the region space is ~90 % live data — the framework objects
that a zygote child gets from the shared pre-fork heap — while the zygote child's own heap is
~90 % launch garbage that its post-fork trim collects. The measured artefact is therefore **not a
GC-timing artefact**: give arm (a) the same GC opportunity and it returns 3.7 MiB/session instead of
43.6; the remaining ~40 MiB/session is the pre-fork shared state doing its job.

Two further facts bound the answer:

* **The image cannot be trimmed at all in (a).** 26.3 MiB of boot image are a private decompressed
  copy per session (ZM1 §3; arm (b) removes exactly that amount), while in (c) the same pages are
  one physical copy apportioned over the parent and every child (`[anon:dalvik-…boot*.art]` 1.7 MiB
  Pss per child in the smaps above, `Shared_Dirty` 36.1 MiB in the parent). No GC can return it.
* **A forced `System.gc()` in arm (a) was not run.** It needs a guest-side hook in
  `LaunchMain` (a product-code change), which this task excludes. What the measurement above shows
  is the closest available equivalent — a real collection in the exec'd guest under the same protocol
  — and it shrinks the region space by 10 %, not 90 %. Because it is a natural (concurrent) rather
  than an explicit GC, it *bounds* rather than equals what a forced trim could return; it does
  establish the point that decides the question: at that moment the exec'd guest's region space is
  ~90 % live, and no collection can return live pages.

### 5.6 The `(d)` control: the heap-policy knob is not a control on this path (negative result)

`ACOS_ART_HEAPGROWTHLIMIT`/`ACOS_ART_HEAPTARGETUTILIZATION` were set to 48m/0.9 and 24m/0.9 with
the intent of forcing the exec'd guest to collect, i.e. as a no-code-change stand-in for "ART runs
the same trim on arm (a)". **They change nothing measurable**, and the reason is a fact about the
seam, not about memory:

| run | knob | app child at 0.5 s | app child at 60 s |
|---|---|---:|---:|
| (a) N=1 | none | 201.3 / 76.6 PD | 200.6 / 75.4 PD |
| (d) N=1 | 48m, util 0.9 | 201.7 / 76.6 | 200.3 / 75.0 |
| (d) N=1 | 24m, util 0.9 | 201.4 / 76.7 | 200.3 / 75.2 |
| (d) N=5 | 48m, util 0.9 | probe: 97.5–102.0 per child over 20 s, no drop | — |

(A 16m/0.9 N=1 run also measured 220.5 MiB whole-tree at 60 s on the console, but it was stopped
during teardown once the seam had proved inert, so no report was kept for it.)

The knob is applied (the session records it: `live-session.json` → `heap.overridden =
["ACOS_ART_HEAPGROWTHLIMIT","ACOS_ART_HEAPTARGETUTILIZATION"]`, `dalvik.vm.heapgrowthlimit = "24m"`),
but it only changes the **served property table**: `launch.rs` never turns `dalvik.vm.heap*` into
`-Xms`/`-XX:HeapGrowthLimit`/`-Xmx` argv, and this guest is started as `dalvikvm64` with an explicit
argv (the `AndroidRuntime::parseRuntimeOption` path that reads those properties belongs to
`app_process`). So the guest's ART never saw the limit. This is reported because it is a caveat for
the W48 seam's advertised reach and because it is the reason the "forced trim" experiment could not
be run without touching product code.

---

## 6. Startup latency and host CPU

All numbers are per run, measured by the driver: "first ready" = the first session's own `ready`
reply (a settled first frame), "all ready" = driver start → every session ready, "CPU-s" = the
whole managed tree's `utime+stime+cutime+cstime` delta over that window (driver excluded).

| arm | N | first session ready (ms) | per extra (ms) | all N ready (ms) | CPU-s | CPU-s excl driver |
|---|---|---:|---:|---:|---:|---:|
| (a) | 1 | 234 | — | 554 | 0.26 | 0.25 |
| (a) | 5 | 245 | 239 | 1415 | 1.28 | 1.26 |
| (a) | 10 | 311 | 221 | 2511 | 2.61 | 2.59 |
| (a) | 20 | 509 | 213 | 4776 | 6.83 | 6.80 |
| (b) | 1 | 646 | — | 951 | 0.26 | 0.25 |
| (b) | 5 | 200 | 240 | 1377 | 1.20 | 1.20 |
| (b) | 10 | 299 | 221 | 2506 | 2.69 | 2.68 |
| (b) | 20 | 461 | 213 | 4705 | 6.66 | 6.65 |
| (c) | 1 | 259 | — | 578 | 0.21 | 0.21 |
| (c) | 5 | 507 | 222 | 1599 | **0.75** | 0.74 |
| (c) | 10 | 703 | 214 | 2830 | **1.54** | 1.54 |
| (c) | 20 | 1220 | 207 | 5361 | **4.40** | 4.38 |

* **Host CPU to bring N sessions to their first correct frame: (c) is 41 % / 41 % / 36 % cheaper
  than (a) at N = 5 / 10 / 20** (0.75 vs 1.28, 1.54 vs 2.61, 4.40 vs 6.83 CPU-s) and 19 % cheaper
  at N=1 (0.21 vs 0.26). Arm (b) saves nothing on CPU (1.20 / 2.69 / 6.66), as expected: the
  shared image removes private *pages*, not the per-session runtime boot.
* **First session ready is (c)'s worst number:** 507 / 703 / 1220 ms at N = 5 / 10 / 20 versus
  245 / 311 / 509 ms for (a) — the driver must publish N−1 session contracts before the zygote can
  fork anything, and that cost grows with N (`spawn_ms` covers all launchers + contracts). It is
  the prototype's control plane, not the fork path: `fork → sandbox+guard ready` was **5 ms** and
  **55 ms** per child in these runs (`fork_to_ready_ms`, tags 3–4 vs 0–2), and the zygote's one-off
  pre-init is inside the first number.
* **Per extra session is identical** in all three arms (207–240 ms): that is the app's own
  first-frame work (~150 ms) plus the driver's sequential read of each session's pipe, not the
  runtime.
* At N=20 (c) is 585 ms slower to have all sessions up (+12 %) while spending 36 % less CPU: it
  serialises contract publication before the forks, then forks cheaply.

---

## 7. Advantages, disadvantages, risks (facts, not a recommendation)

**Measured advantages of (c) over the optimised current path**

1. **Whole-system footprint**: at N ≥ 10 it is 59.6–67.1 % below (a) and 55.4–62.5 % below (b) at
   every time point; at N = 5 it is 45.8–46.5 % below (a) and 44.3–45.2 % below (b) from 15 s on.
2. **Marginal cost of one more session**: 22.2 MiB (c) vs 80.9 (a) / 65.4 (b) at 60 s — 72.6 % /
   66.1 % less. The guest side of the marginal is 12.2 vs 76.2 / 60.7 MiB.
3. **Per-session private memory**: a (c) child holds 11–16 MiB Private_Dirty (17–89 MiB Pss
   depending on how many processes share the image), an (a) child 74–78 MiB PD. Nothing in the
   inherited shared set erodes (0.08 MiB/child per session, ZM1; confirmed here from the other
   direction — the shared mappings do not move during the release, §5.2).
4. **Host CPU**: 36–41 % less to bring N sessions to their first correct frame at N ≥ 5 (4.40 vs
   6.83 CPU-s at N=20; 0.75 vs 1.28 at N=5).
5. **No quality loss**: every correctness, isolation and pixel check passes at every N (§8), and the
   release of §5 happens in sessions that remain fully interactive (post-settle tap → `2`).

**Measured disadvantages of (c)**

1. **N = 1 is a loss at 0.5 s and a small win in steady state**: −7.6 % at 0.5 s, +4.7 % to
   +6.9 % at 15/60 s versus (a).
   The ~98 MiB pre-fork core (77.8 MiB zygote parent + 20.0 MiB research helpers) is paid for one
   child. The crossover is between N=1 and N=5.
2. **A ~10 s settling window after each first frame** (9.2–10.2 s, §5.1) in which the child is at
   1.7–2× its steady-state size. A workflow that samples memory, measures a container, or decides
   admission inside those 10 s sees the pre-release number.
3. **Slower to have all sessions up**: first session ready 507/703/1220 ms vs 245/311/509 ms, all
   N ready +13 % at N=20 (5 361 vs 4 776 ms) — the prototype publishes N−1 contracts before the
   zygote forks.
4. **Per-session launcher cost is 2× (a)'s** (10.6 vs 4.6 MiB at 60 s; +4.6 MiB/session accumulating
   at ~78 KiB/s of anonymous mapping, §4.1) — a seam property that eats 34 % of (c)'s N=20 total.
5. **Cannot be stacked with (b)** (ZS F11), so the two measured wins add to nothing; (b) alone is
   9–15 % better than (a) at N ≥ 10 and (c) was measured against (b) as well as against (a).
6. **Single point of failure**: one zygote forks, and therefore one zygote crash, GC stall or OOM
   takes every session in the tree (and all children share launcher 0's cgroup, ZS F3). Teardown in
   these runs was a kill: `close` on a (c) session takes the shared zygote down (`launcher_exit -9`).
7. **Hidden-API and guard deviation**: `-Xzygote -Xhidden-api-policy:disabled` on the zygote argv,
   a research guest dex, a reflective `Daemons.start()`, and a guard `.so` rebuilt with
   `-fno-stack-protector` (Z4 §6.5, ZS F7).

**Risks that this measurement does not reduce**

1. **Security (untrusted)**: F1 is proven reachable at every N measured here
   (`sibling_sock … read=true,write=true`), and F2/F3/F8/F9/F10 are unchanged. ZS excludes the
   zygote path for untrusted APKs independently of any memory number (§9).
2. **Security (trusted, multi-session)**: F4/F5/F6/F7 must land first; the "nothing session-specific
   before `preFork`" invariant is review-only.
3. **Compatibility**: one fixture app, x86_64, one host. The zygote path has not been shown to run a
   form, a dialog flow, a React Native or Gaten app, a repeated launch→teardown→relaunch, or an
   allocation-heavy workload — and the release of §5 means a heavier app's post-fork GC could pause
   at ~10 s where an exec'd app would not GC at all.
4. **The release is policy, not law**: it exists because ART's post-fork path shrinks the target
   footprint to `dalvik.vm.heapstartsize` (8 MiB) 10 s after the fork. A product that changes that
   property changes the size *and* the timing of the win measured here.
5. **Measurement surface**: single run per cell on a host shared with another agent's ART runs
   (queue waits up to 31 min); the 15 s/60 s growth of (a)/(b) should be read as ±2 %; the (c)
   launcher growth is real but its allocation site is not identified.

---

## 8. Correctness, isolation, swap

**Every configuration passed every check** — 17 configurations (a/b/c × N = 1/5/10/20, the (c) N=2
smoke, and the five probe/control runs), **125 sessions**, each one checked for `0` → tap → `1`, its
isolation reads, its pixels, and (all 125) the 60 s post-settle repeat.

| check | result |
|---|---|
| `:id/count` on the first settled tree | `0` in every session of every configuration |
| real InputChannel tap on `:id/inc` | `1` re-read from a **new settled frame** in every session |
| ordered isolation matrix | every not-yet-tapped session re-read `0` after each tap — 0 / 10 / 45 / 190 checks at N = 1 / 5 / 10 / 20, `failures=0` |
| C1 pixels | all sessions byte-identical at both steps: md5 `39d1e4e3633d98d76b992edb4b250a78` initial, `9f3406c1627beef270f8cfddf9f61973` after the tap — the same two md5s as Z3/Z4/Z5, in arms (a), (b) and (c) |
| post-settle liveness (60 s sample first, then tap) | `2` in the tapped session and `1` in every other, at every N of every arm (125 sessions) — the release of §5 does not degrade a live session |
| swap | `ΣSwapPss = 0.000 MiB` and `ΣSwap = 0.000 MiB` in every process of every configuration; `memory.swap.max=0`, `memory.swap.current=0` on every sampled scope; `memory.events` is `max=0, oom=0, oom_kill=0` on every scope of every configuration (17 configurations, all samples); `oom.group=0` (the host's `app.slice` default, ZS F3) |
| host RAM | read before every run: 30–38 % used of 61 908 MiB (N=20 peak ≈ 2.0 GiB reserved tree) |

Teardown, recorded but not a gate criterion: in these runs **every** launcher of every arm was
still alive 20 s after `{"cmd":"close"}` and was killed by the driver's bound (`launcher_exit`
`-9`). Z5 recorded exit `0` for arms (a)/(b) and `-9` only for (c); the difference is host load
(another agent's ART runs were queued/running throughout, §10) and it cannot change a memory
number, because every sample is taken before teardown.

---

## 9. Security: unchanged from ZS

Z6 changed **no** sandbox line: the only launcher-side code this track touched is behind
`ACOS_ZYGOTE_*`/`ACOS_ART_MALLOC_*` knobs that no production path sets (Z4/Z5 seam, unchanged by
Z6). The security status of the zygote path is therefore **exactly ZS's**, and the Z4SEC probes
re-ran in every (c) configuration of this batch:

| ZS item | ZS verdict | Z6 evidence (fresh, this batch) |
|---|---|---|
| child confinement at N=1 | identical to the exec path (same bwrap args, seccomp blob, Landlock plan, cgroup) | `guard_ms=0 seccomp=2`, `OK guard=active`, `OK landlock=active` in every child's `run/out/seccomp.log` |
| sibling `/proc`, signalling (F2) | `kill` denied and `/proc` invisible; `tgkill`/`rt_sigqueueinfo` unfiltered | `proc_sibling=false`, `proc_zygote=true` (the pid-file number is the child itself, PID 2), `kill_sibling=-13`, `kill_zygote=-13` in every child, at N = 1/5/10/20 |
| cross-session sockets/files (**F1**) | **real finding, not fixed** — the plan dir is bound at `/sessions` in every child | `plan_visible=true`, `sibling_sock=/sessions/<k>/sock/peer.sock:exists=true,read=true,write=true` in every child, at N = 1/5/10/20 |
| cgroup / OOM (**F3**) | one scope for the zygote + all children | confirmed again: launcher 0's scope holds the zygote, every `z4ns`/`z4init` and every app child; per-scope `memory.oom.group=0` on this host |
| inherited state (F4–F7) | logd fd, pre-fork static, signal dispositions, `-Xhidden-api-policy:disabled` | unchanged; the zygote argv still carries `-Xzygote -Xhidden-api-policy:disabled` |
| identity / layout (F8, F10) | no uid boundary unprivileged; all children share the pre-fork layout | unchanged (`userns=0 mntns=0 pidns=0`, `NSpid=2` per child) |
| (b)+(c) stacking (F11) | `-Ximage` cannot be combined with `-Xzygote` | arms (b) and (c) ran as separate arms only; no configuration in this report contains both |

Consequences, unchanged: for **untrusted** APKs ZS blocks the zygote path on F8/F9/F10 (and F1–F3
at N>1) regardless of any memory number, so the gate's "without sacrificing … security" condition
is **not met for untrusted workloads at any N**; for **trusted** first-party APKs ZS's conditions
are F4/F5/F6/F7 (and, for multi-session, F1 + F2/F3 + F13 evidence, which this batch extends to
N = 20 for the correctness/isolation half only). Z6's own additions are two unregistered research
knobs (`ACOS_ZYGOTE_NO_STUB` from Z5, `--release-probe` is harness-only) — ZS F12 still applies.

---

## 10. What was not measured

1. **One fixture, one host, one repetition per (arm, N, t) cell.** `counter.apk` is the lightest
   real app; no dialog/form/React Native/Gaten fixture, no sustained allocation load, no
   repeated launch→teardown→relaunch on arm (c) (Z4 §6.7's open list is unchanged).
2. **Host contention.** Another agent's ART measurements held the shared lock for most of this
   batch (queue waits of 0–31 min per acquisition, `art-locks/runs.log`); one Z6 acquisition was
   cut by its own guard at 2 400 s and the remaining (c) configurations were re-run separately.
   The three time points of every cell come from one run, so contention cannot skew a *row*, but
   it is why launcher teardown timed out everywhere (§8) and why the 15 s/60 s growth of arms
   (a)/(b) (which is real, §4) should be read as ±2 %.
3. **The (c) launcher's own growth** (4.0 → 8.6 MiB Pss between 0.5 s and 60 s, +4.1 MiB per
   session over arm (a)'s launcher) is measured and attributed to a mapping (§4.1) but its cause
   is not fixed — it is a property of the Z4 foreign-guest seam (the second `acos-art` per
   session that does nothing but hold the session), not of the fork.
4. **Per-session uid, SELinux, per-child cgroup** (ZS F8/F3) are not implemented, so no
   measurement here says anything about them.
5. **CPU of the steady state** (only bring-up CPU is measured; the 60 s idle CPU was not
   sampled) and **energy** are not measured.
6. **Anything about a production zygote** — zygote-per-ABI, `ZygoteInit`/`RuntimeInit` as the
   post-fork entry, a launcher that owns the session lazily instead of a holder process, or a
   per-child mount namespace — is design, not measurement.

---

## 11. Reproducing

```bash
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export CARGO_TARGET_DIR=/home/Qui/Projects/acos/.cache/z6-target
CARGO_BUILD_JOBS=4 cargo build --release -p acos-cli -p acos-art   # main (W1/P9/P10) + the Z4/Z5 seams

# the matrix: three arms x N = 1/5/10/20, 0.5/15/60 s in one run each
tools/art-runtime/run-exclusive.sh --task Z6 -- tools/guarded.sh --mem 24G --time 3600 -- \
  results/art-runtime/evidence/Z6/run-z6.sh a b c
# the release mechanism: (c) and (a) at N=5 with the 20 s 1 Hz probe
tools/art-runtime/run-exclusive.sh --task Z6 -- tools/guarded.sh --mem 24G --time 1500 -- \
  results/art-runtime/evidence/Z6/run-z6-probe.sh
# the heap-policy control (kept for the record: the knob does not reach this guest, §5.6)
tools/art-runtime/run-exclusive.sh --task Z6 -- tools/guarded.sh --mem 24G --time 1200 -- \
  results/art-runtime/evidence/Z6/run-z6-heapctl.sh
# tables
python3 results/art-runtime/evidence/Z6/tables.py        # table.txt
python3 results/art-runtime/evidence/Z6/gate_md.py       # the markdown blocks in this file
bash    results/art-runtime/evidence/Z6/collect.sh       # gzip the reports into evidence/Z6/raw
```

One `guarded.sh --time` per acquisition must cover the whole batch: at N=20 one configuration
costs ~10 min (the 190-check isolation matrix, the 60 s sample and the post-settle matrix), so the
three-arm matrix needs ≥ 60 min. In this batch it was cut once at 2 400 s (after b-N20, §10.2) and
the (c) configurations were re-run in three separate acquisitions; total locked ART time for every
number in this file was ~80 min, plus 3.5 h of queue wait for the shared lock.

Evidence in git: `evidence/Z6/{run-z6.sh,run-z6-probe.sh,run-z6-rest.sh,collect.sh,tables.py,
gate_md.py,summary.json,table.txt.gz}` and `evidence/Z6/raw/*.json.gz` (the driver reports: per
session replies, per-process memory rows with SwapPss, cgroup files, the 1 Hz release-probe
series). Raw guest/launcher logs stay in `.cache/z6-run/` (not in git; 678 MiB — the per-session
`live-frames/` PNG directories were deleted after the batch, and every committed number comes from
the JSONs above).
`CARGO_TARGET_DIR` was deleted after the report.
