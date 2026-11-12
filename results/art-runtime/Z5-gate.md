# Z5 — the owner's 25 % gate, measured on three arms at N = 1 / 5 / 10 / 20

**Gate under test (verbatim).** *"Only recommend production integration if the new architecture
demonstrates at least a 25 % whole-system improvement over the optimised current implementation,
without sacrificing correctness, security or compatibility."*

This file is measurement only. It states the numbers, the advantages, disadvantages and risks, and
the threshold verdicts as facts; it does **not** make the recommendation.

Worktree `zygote-z5-gate` (branch `zygote-z5-gate`, main + Z4 + B3 + P1–P4/P3b/P8). Time box: 60 min
planned, ~2.5 h used, because the measurement kept producing gate-relevant findings (§3).

---

## 0. Facts up front

| # | fact |
|---|---|
| 1 | Z4's 9.1 MiB/session tombstone stub is **gone from the measurement**: an env-gated, OFF-by-default `ACOS_ZYGOTE_NO_STUB` makes the foreign-guest session's liveness token a bare host `/bin/sleep` with no bwrap, no mount view and no `LD_PRELOAD`. Measured residual: **0.14 MiB Pss/session** (was 9.1). |
| 2 | All three arms, all N, **pass every correctness check**: `0` → tap → `1`, the full ordered isolation matrix (0/10/45/190 checks at N=1/5/10/20), and **byte-identical C1 pixels** (`39d1e4e3…` / `9f3406c1…`, the same md5s as Z3/Z4). |
| 3 | Under Z4's own method (memory sampled 0.5 s after the last tap) the zygote arm is **−8.8 % at N=1, +8.3 % at N=5, +52.1 % at N=10, +56.8 % at N=20** whole-system versus the optimised current path; marginal 31.3 vs 99.2 MiB/session. |
| 4 | **The 0.5 s snapshot is not internally consistent for arm (c).** The zygote's app children release ~45 MiB each after ≈10 s of idleness; the exec'd guests of arms (a)/(b) do not release anything even after 60 s. So a 0.5 s snapshot catches (c) *before* the release at N=5 and *after* it at N=10/20. Under one consistent protocol (60 s settle — D-M2's own method) both arms: **N=5 zygote 378.3 vs 641.1 MiB = +41.0 %; N=10 591.0 vs 1147.1 MiB = +48.5 %**. At N=20 the Z4-method run is already a settled reading for the early children (913.3 vs 2111.8 = +56.8 %). |
| 5 | The 25 % whole-system threshold is **not met at N=1** (worse), **not met at N=5 under the 0.5 s protocol** (+8.3 %), and **met at N=5/10 under the 60 s steady-state protocol** (+41.0 % / +48.5 %) and in the Z4-method N=10/20 rows (§2). Versus (a)+(b) — the current path *with* B3's shared boot image — the numbers are 2–8 points lower, because (b) itself saves 6–9 %. |
| 6 | Security is **unchanged by Z5 and unchanged from ZS**: untrusted APKs stay on the exec path; the multi-session zygote still carries ZS's F1 (cross-session sockets, re-confirmed reachable by the Z4SEC probe at N=5), F2/F3 (shared pidns/cgroup), F4–F7 (inherited fd/statics/signals/hidden-API policy), F8/F10 (no uid boundary, shared layout). |
| 7 | Arms (b) and (c) still **cannot be stacked** (ZS F11), so no measured configuration contains both optimisations; the “(a)+(b) combined” comparison below is (b) alone, which *is* the optimised current path plus the shared image. |

---

## 1. Method (identical for all three arms)

One harness: Z4's `research/zygote-proto/z4/drive.py` — same protocol, same checks, same
`smaps_rollup` sampling — with three Z5 additions, each of which changes no existing number
(`--settle`, host-CPU accounting, a post-settle liveness/isolation pass; all default-off or
additive). One binary: `CARGO_TARGET_DIR=/home/Qui/Projects/acos/.cache/z5-target cargo build
--release -p acos-cli -p acos-art` = main + the Z5 seam of §2, nothing else. Every ART run went
through `tools/art-runtime/run-exclusive.sh --task Z5 -- tools/guarded.sh --mem 24G --time
600|840|900 -- …`; scratch stayed in `/home/Qui/Projects/acos/.cache/z5-*`; `free -m` was read before
every run (host used 20.6–25.4 GiB of 60.5 GiB = 34–42 %, available 37.5–41.2 GiB, so N=20 was run).

Per configuration: N sessions of `fixtures/build/apps/counter.apk`, each a real
`acos-art launch --apk … --until activity-resumed --interactive` with its own
ServiceHost/InputHost/C1/sockets/run root; the driver waits for each session's own `ready` line,
reads the tree (`:id/count` = `0`), taps `:id/inc` with a real InputChannel tap, re-reads the count
on a new settled frame (`1`), re-reads every not-yet-tapped session (`0`), and screenshots; then,
with all N sessions held, it samples `/proc/<pid>/smaps_rollup` for every process in its tree.

| arm | path | env |
|---|---|---|
| **(a) optimised current** | main default | **no `ACOS_*` seam set** |
| **(b) shared boot image (B3)** | (a) + uncompressed image, `-Xnorelocate` | `ACOS_SHARED_BOOT_IMAGE=.cache/b2/image` |
| **(c) zygote (Z4 seam)** | one pre-initialised ART zygote forks one child per session; LZ4 image | `ACOS_ZYGOTE_SESSION/APP/PLAN_DIR/N` + `ACOS_ZYGOTE_NO_STUB=1` |

Definitions:

* **whole system** = every process in the driver's tree **except the driver**: the N `acos-art`
  launchers, the N `bwrap` sandboxes, the guests (or the zygote + its forked children), the Z4
  `z4ns`/`z4init` helpers and the per-session holders. That is the brief's definition, and it keeps
  the python harness out of the comparison (its Pss is 11–26 MiB and grows with N because it holds
  N reply buffers). With-driver totals are in `evidence/Z5/summary.json` for comparability with
  Z4's published numbers.
* **ΣPss** = Σ`Pss` (`smaps_rollup`), shared pages apportioned. **ΣPD** = Σ`Private_Dirty`.
* **marginal** = least-squares slope over N = 1/5/10/20, MiB per extra session.
* **% saving** = (arm_a − arm_x)/arm_a at the same N; **positive = the arm uses less memory** than
  arm (a), negative = it uses more (so arm (b)'s N=1 row reads −12.9 %).
* **protocol**: Z4's method samples 0.5 s after the last tap; the steady-state method samples after
  60 s (D-M2's `--settle 60`). Both are reported, and §3 explains why the difference is not noise.

---

## 2. The tombstone stub: removed, not subtracted

Z4 kept each non-zero session alive with a *tombstone guest* — `/system/bin/sleep` **inside** that
session's bwrap, inheriting the production `LD_PRELOAD` of four shared libraries. From Z4's own
evidence (`.cache/z4-run/zygote-5.json`): **9.1 MiB Pss, 4.89 MiB private dirty, 32.8 MiB RSS per
session** (36.4 MiB of the N=5 total) — the whole difference between Z4's 29.4 % guest-tree
marginal and its 15.3 % whole-system marginal.

Z5 replaces it, env-gated and OFF by default (`crates/acos-art/src/launch.rs`, the foreign-guest
spawn branch, ~12 lines + comment): with **`ACOS_ZYGOTE_NO_STUB=1`** the session's liveness token is
a bare host `/bin/sleep 1800` — no bwrap, no mount view, no `LD_PRELOAD`, no seccomp/Landlock stub,
nothing app-visible. Measured residual: **0.14 MiB Pss and 0.00 MiB private dirty per session**
(98.5 % less than the tombstone). The branch requires `ACOS_ZYGOTE_SESSION` set **and**
`ACOS_ZYGOTE_APP` unset, so the production path cannot reach it; arm (a) below *is* the production
path and built byte-identical launches (same md5s, §5).

For continuity both readings are given: every table uses **arm (c) as measured** (0.14 MiB holder),
and §2.5 reconstructs *arm (c) with Z4's tombstone* as c + 9.1 × (N−1) MiB from Z4's measured stub
Pss, clearly labelled. To also remove the 0.14 MiB holder, subtract 0.14 × (N−1) MiB.

The holder is a measurement placeholder for the product change Z4 §6.1 describes
(`Option<Child>` + a session-owned liveness source); it does not add a guest, so it does not change
what the architecture costs.

### 2.1 Per-N, MiB (Z4's method, memory sampled 0.5 s after the last tap)

| N | arm | ΣPss | ΣPrivate_Dirty | guest side ΣPss | launchers | bwrap | holders | verdict |
|---:|---|---:|---:|---:|---:|---:|---:|---|
| 1 | (a) | 226.4 | 96.8 | 202.3 | 23.8 | 0.3 | 0.0 | pass |
| 1 | (b) | 255.7 | 83.1 | 231.6 | 23.8 | 0.3 | 0.0 | pass |
| 1 | (c) | 246.4 | 65.5 | 222.5 | 23.9 | 0.0 | 0.0 | pass |
| 5 | (a) | 624.5 | 488.9 | 520.7 | 102.6 | 1.2 | 0.0 | pass |
| 5 | (b) | 585.9 | 406.8 | 482.0 | 102.6 | 1.2 | 0.0 | pass |
| 5 | (c) | 572.4 | 388.3 | 469.3 | 102.5 | 0.0 | 0.6 | pass |
| 10 | (a) | 1115.8 | 978.7 | 902.9 | 210.6 | 2.3 | 0.0 | pass |
| 10 | (b) | 1020.7 | 839.5 | 807.6 | 210.9 | 2.3 | 0.0 | pass |
| 10 | (c) | 534.7 | 353.2 | 322.8 | 210.6 | 0.0 | 1.2 | pass |
| 20 | (a) | 2111.8 | 1972.4 | 1628.2 | 479.2 | 4.4 | 0.0 | pass |
| 20 | (b) | 1920.4 | 1736.6 | 1435.4 | 480.5 | 4.4 | 0.0 | pass |
| 20 | (c) | 913.3 | 725.4 | 426.2 | 484.6 | 0.0 | 2.5 | pass |

Arm (a) rows are the **warm repeat** (`a-N-r2`): arm (a)'s first run of the session paid a one-off
`dex2oat` build-cache miss (`build-cache STORE 741965f70d0d`, absent from every later run) which
inflated only that run's ready time and CPU. Run-to-run scatter between the two arm-(a) passes:
−3.7 / +20.0 / +36.4 / −57.9 MiB at N = 1/5/10/20 (i.e. ≤ 3 %).

### 2.2 Marginals, MiB per extra session

| arm | ΣPss/session (least squares over N=1…20) | ΣPss/session (endpoints) | guest-side ΣPss/session |
|---|---:|---:|---:|
| (a) | 99.2 | 99.2 | 74.8 |
| (b) | 87.9 | 87.6 | 63.4 |
| (c) | 31.3 | 35.1 | 6.7 |

Arm (c)'s *marginal* is the number most distorted by §3: with the 0.5 s protocol its N=1→5 endpoint
slope is (572.4 − 246.4)/4 = **81.5 MiB/session**, i.e. 17.8 % below (a)'s; the 31.3 figure is
dominated by the released state of the N=10/20 children.

### 2.3 Savings at the same N (Z4's method)

| N | (b) vs (a) | (c) vs (a) | (c) vs (a)+(b) | (c) vs (a), guest side | (c) vs (a), ΣPD |
|---:|---:|---:|---:|---:|---:|
| 1 | −12.9 % | −8.8 % | +3.6 % | −10.0 % | +32.3 % |
| 5 | +6.2 % | +8.3 % | +2.3 % | +9.9 % | +20.6 % |
| 10 | +8.5 % | +52.1 % | +47.6 % | +64.2 % | +63.9 % |
| 20 | +9.1 % | +56.8 % | +52.4 % | +73.8 % | +63.2 % |

### 2.4 Reconstructed: arm (c) carrying Z4's tombstone instead of the holder

| N | (c) measured | (c) + 9.1×(N−1) | vs (a) | vs (a)+(b) |
|---:|---:|---:|---:|---:|
| 1 | 246.4 | 246.4 | −8.8 % | +3.6 % |
| 5 | 572.4 | 608.8 | +2.5 % | −3.9 % |
| 10 | 534.7 | 616.6 | +44.7 % | +39.6 % |
| 20 | 913.3 | 1086.2 | +48.6 % | +43.4 % |

This is the number Z4 could not report honestly: **without the tombstone, the zygote's N=5
whole-system figure is 8.3 % (0.5 s protocol) / 41.0 % (steady state), not 6.6 % / 15.3 %.**

---

## 3. Protocol sensitivity: the single most important finding, and the one open item

The three arms' per-session memory is not a constant after the session is ready. Measured per app
child (Pss / Private_Dirty, MiB), same fixture, same work, same host:

| arm | N=1 | N=5 | N=10 | N=20 | after 60 s idle at N=5 | at N=10 |
|---|---:|---:|---:|---:|---:|---:|
| (a) exec'd guest | 121 / 78 | 105 / 77 | 91 / 77 | 88 / 77 | **105 / 77** (no change) | **89 / 76** |
| (b) exec'd guest + shared image | — | 96 / 63 | 84 / 65 | 76 / 66 | — | — |
| (c) zygote child | 121 / 46 | 78 / 58 | 24 / 14 | 17 / 12 | **35 / 16** | **26 / 16** |

Facts that pin the effect down:

* it is **not** idle-sampling noise and **not** generic kernel reclaim: arm (a) sits for a full
  60 s with **zero** change (105/77 → 105/77 MiB at N=5, 91/77 → 89/76 at N=10), on the same host,
  minutes apart from the (c) runs;
* it is **time-in-session and zygote-specific**: (c)'s children release ~45 MiB of private-dirty
  heap each ≈10 s after their first frame, and the release has happened by the time a 20-session
  sequence finishes tapping (hence (c)'s 17 MiB at N=20 even in the 0.5 s protocol);
* it is **not a dead or degraded session**: with the release in effect, a *further* tap on every
  session still yields `2` in that session and `1` in every other (post-settle pass at N=5, N=10;
  `failures=0`), and pixel md5s stay the same;
* the launcher side is unaffected and identical across arms at steady state (N=5: 126.7 MiB (a) vs
  125.8 MiB (c)).

Mechanism: **not established in the time box.** Two candidates fit the evidence — ART's
post-fork/idle GC with compaction and `madvise` of the region space (the shared, pre-fork heap is
trimmed back to its pre-fork size), or kernel reclaim of the zygote children's idle pages under the
single shared scope (ZS F3). The exec'd guests showing no release at all is what makes the ART
explanation the more likely one (a zygote child's first GC after `postForkChild` is a full
compaction), but the discriminating measurement — sampling `VmSwap` / `memory.stat` / `memory.events`
of launcher 0's scope *during* a 60 s settle — was not run.

Why it decides the gate wording: with one consistent protocol the comparison is
**(c) 378.3 vs (a) 641.1 MiB at N=5 = +41.0 %** and **591.0 vs 1147.1 MiB at N=10 = +48.5 %**,
while the 0.5 s protocol's +8.3 % at N=5 is a snapshot of
(c) taken *before* its own steady state. Conversely, the Z4-method N=10/20 rows (52 %/57 %) are
snapshots taken *after* it. This is not an inconsistency of the arms: it is one protocol applied to
two different settling behaviours, and it is the reason both protocols are reported.

### 3.1 Steady state (60 s settle after the last tap, then the memory sample, then a liveness pass)

| N | (a) ΣPss | (a) ΣPD | (c) ΣPss | (c) ΣPD | saving | (c) vs (a)+(b) | post-settle liveness |
|---:|---:|---:|---:|---:|---:|---:|---|
| 5 | 641.1 | 505.6 | 378.3 | 200.0 | **+41.0 %** | +35.4 % (vs (b) 585.9)¹ | pass (tap → 2 ×5, isolation ×10) |
| 10 | 1147.1 | 1009.8 | 591.0 | 409.0 | **+48.5 %** | +42.1 % (vs (b) 1020.7)¹ | pass (tap → 2 ×10, isolation ×45) |

¹ (b)'s steady state was not measured; its 0.5 s figure is the *conservative* denominator, because
(b)'s steady state is slightly higher than its 0.5 s state (arm (a) on the same protocol change:
+2.7 % at N=5, +2.8 % at N=10).

Steady-state marginals over N=5→10: **(c) 42.5 MiB/session vs (a) 101.2 MiB/session, 58 % less**.
Steady-state host CPU: 0.84 vs 1.24 CPU-s at N=5 and 1.73 vs 2.80 at N=10 (32 % / 38 % less).
First-session ready: 517 vs 235 ms at N=5 (the contract-publication cost of §4), 286 vs 287 ms at
N=10. Launcher Pss is identical across arms (125.8 vs 126.7 MiB at N=5, 257.7 vs 256.9 at N=10),
i.e. the whole difference is on the guest side (252.0 vs 513.2, 332.0 vs 887.8 MiB).

(a)'s steady state is *higher* than its 0.5 s state (+16.6 MiB at N=5, +31.3 at N=10): nothing is
released, and the launcher's frame history grows. (c)'s steady state is *lower* by ~194 MiB at N=5
and ~556 MiB at N=10.

---

## 4. Timing and host CPU (Z4's method)

| N | arm | first session ready (ms) | per extra session ready (ms) | all N ready (ms) | host CPU-s to bring up N |
|---:|---|---:|---:|---:|---:|
| 1 | (a) | 178 | — | 489 | 0.20 |
| 1 | (b) | 663 | — | 968 | 0.27 |
| 1 | (c) | 239 | — | 544 | 0.24 |
| 5 | (a) | 212 | 220 | 1 299 | 1.11 |
| 5 | (b) | 236 | 236 | 1 396 | 1.24 |
| 5 | (c) | 485 | 225 | 1 601 | **0.81** |
| 10 | (a) | 292 | 210 | 2 391 | 2.54 |
| 10 | (b) | 274 | 220 | 2 472 | 2.77 |
| 10 | (c) | 266 | 222 | 2 483 | **1.66** |
| 20 | (a) | 449 | 210 | 4 653 | 7.69 |
| 20 | (b) | 416 | 212 | 4 649 | 7.26 |
| 20 | (c) | 368 | 252 | 6 028 | **4.71** |

* **Host CPU to bring N sessions to their first correct frame**: (c) is **27–39 % cheaper** than (a)
  at every N ≥ 5 (0.81 vs 1.11 s at N=5; 1.66 vs 2.54 at N=10; 4.71 vs 7.69 at N=20) — the zygote
  does not re-run `Runtime::Start` (classpath, JNI table, system `Resources`) per session. Arm (b)
  saves nothing on CPU, as expected.
* **First session ready**: (c) is slower at N≥5 (485 vs 212 ms at N=5, 368 vs 449 ms at N=20). The
  driver must publish N−1 session contracts before the zygote can fork anything
  (`spawn_ms` 257 ms at N=5 vs 5.9 ms for (a)), and the zygote's one-off pre-init is inside that
  number. This is the prototype's control plane, not the fork path: `fork → sandbox+guard ready`
  was 4–6 ms per child (one run showed 55–57 ms for its first three children on a host that was
  also building; Z4 measured 5 ms).
* **Per-extra-session ready** is equal (~210–250 ms) in all arms because the driver readies
  sessions one at a time; it is dominated by the app's own ~150 ms first frame, not by the runtime.

---

## 5. Correctness at every N (identical checks for all three arms)

| configuration | sessions | `0` → tap → `1` | isolation checks | md5 initial | md5 after tap | verdict |
|---|---:|---|---:|---|---|---|
| (a) N=1/5/10/20 | 36 | all | 0 / 10 / 45 / 190 | `39d1e4e3…` | `9f3406c1…` | pass ×4 |
| (b) N=1/5/10/20 | 36 | all | 0 / 10 / 45 / 190 | same | same | pass ×4 |
| (c) N=1/5/10/20 | 36 | all | 0 / 10 / 45 / 190 | same | same | pass ×4 |
| (c) steady state N=5, N=10 | 15 | `1` → settle → tap → `2` in-session, others stay `1` | 10 / 45 | same | same | pass ×2 |

* Every session read `count=0` on its first settled tree and `1` after its own tap; every
  not-yet-tapped session re-read `0` after each tap (the ordered matrix, 0/10/45/190 checks).
* **Pixels:** all 108 sessions across the twelve configurations produced byte-identical PNGs at both
  steps and the same two md5s as Z3/Z4 — the zygote path is the same C1 buffer generation, and so is
  the shared-image path.
* Arm (c)'s steady-state sessions were **tapped again after the 60 s settle** (→ `2`, isolation
  intact), so the memory release of §3 does not degrade a live session.
* **Teardown:** arms (a)/(b) launchers exit `0`. Arm (c)'s exit `-9`: closing a session kills the
  shared zygote it spawned (the parent of all app children), and two of ten launchers at N=10 (and
  the first `c-10-s60` attempt) then sat in a futex wait until the driver's bound killed them — a
  real teardown defect of the prototype, bounded (no orphan survived any run) but not graceful.
* Build/tests: `cargo test --offline -p acos-art --lib` (through the shared ART lock, Z5's own
  build tree): **82 passed, 0 failed, 0 ignored** — the seam is inert unless
  `ACOS_ZYGOTE_SESSION` is set, and arm (a) above is the unmodified production path.

---

## 6. Security (ZS is the gate input; Z5 changes no sandbox layer)

ZS (`results/art-runtime/ZS-security-review.md`) stands unchanged: **untrusted third-party APKs stay
on the existing exec path.** Nothing in Z5 touches seccomp, Landlock, bwrap, cgroup or the guard; the
only launcher change is the env-gated holder of §2, unreachable without `ACOS_ZYGOTE_SESSION`.

| item | ZS verdict | Z5 evidence (fresh) |
|---|---|---|
| child confinement at N=1 | identical to the exec path (same bwrap args, same seccomp blob, same Landlock plan, same cgroup) | production guard active in every child; (c) N=1 passes |
| sibling `/proc`, signalling | `kill` denied, `proc` invisible per child; `tgkill`/`rt_sigqueueinfo` unfiltered (F2) | Z4SEC at N=5/10/20: `proc_sibling=false`, `kill_sibling=-13`, `kill_zygote=-13` |
| cross-session sockets/files (**F1**) | **real finding, not fixed**: the plan dir is bound at `/sessions` in every child | `plan_visible=true`, `sibling_sock=/sessions/1/sock/peer.sock:exists=true,read=true,write=true` at N=5, N=10, N=20 |
| cgroup / OOM (**F3**) | one scope for the zygote + all children; `oom.group` shared | unchanged (launcher 0's scope holds the zygote and every child) |
| inherited state (F4–F7) | logd fd, pre-fork static (`sOverlayIdmaps`), signal dispositions, `-Xhidden-api-policy:disabled` | all four unchanged |
| identity / layout (F8, F10) | no uid boundary unprivileged; all children share the pre-fork layout | unchanged |
| (b)+(c) stacking (F11) | `-Ximage` cannot be combined with `-Xzygote` | arms (b) and (c) ran as separate arms only |

Conditions for **trusted** workloads (ZS §7): F4 (fd hygiene, per-child logd, `SOCK_CLOEXEC`),
F5 (nothing session-specific before `preFork`), F6 (clean signal dispositions), F7 (drop
`-Xhidden-api-policy:disabled`), and for multi-session additionally **F1** (per-child mount ns +
per-session `/data`), **F2+F3** (per-child pidns or filtered `tgkill`/`rt_*sigqueueinfo`, per-child
cgroup scope with `oom.group`), and **F13** (N>1 evidence — Z5 supplies the correctness/isolation
half at N up to 20; the adversarial half is still only the Z4SEC probe, which is why F1 reads
`plan_visible=true` here). For **untrusted** workloads ZS blocks on F8, F9, F10, F11 regardless of
any memory number. Z5's own seam adds one item: an unregistered env knob with a side effect, which
inherits ZS F12 (register the zygote knobs in `docs/SECURITY.md`).

---

## 7. Advantages, disadvantages, risks

**Measured advantages of (c)**

1. Marginal whole-system cost per extra session: **81.5 MiB (0.5 s protocol, N=1→5) or 31.3 MiB
   (above) vs 99.2 MiB** for the current path; guest-side marginal 6.7 vs 74.8 MiB/session.
2. Steady-state footprint per session: app child **35 MiB Pss / 16 MiB PD** vs **105 / 77** at
   N=5 and **26 / 16** vs **89 / 76** at N=10 — the largest effect measured in this track; it grows
   with N (17 / 12 MiB at N=20).
3. **27–39 % less host CPU** to bring N sessions up, at every N ≥ 5 (32–38 % on the steady-state
   runs: 0.84 vs 1.24 CPU-s at N=5, 1.73 vs 2.80 at N=10).
4. No per-session runtime boot: the ~40 MiB pre-fork core (boot image, framework classes, JNI table,
   system `Resources`) is paid once and shared.
5. No quality loss in any check at any N (§5); no change to C1 bytes.

**Measured disadvantages of (c)**

1. **N=1 is a loss** (−8.8 % whole system, 0.5 s protocol): the core is paid and there is no second
   child to divide it over. Single-session tasks — the common case — get worse.
2. First-session ready time is worse at N≥5 (485 vs 212 ms) because N−1 contracts must be published
   before the zygote forks; the prototype's control plane, not the fork path.
3. The savings are dominated by a settling behaviour that the current method does not exercise for
   arm (a) (§3); until its mechanism is identified, any percentage is a statement about *when* you
   look.
4. It cannot be stacked with (b) (F11), so the two measured wins cannot be added.
5. Teardown is a SIGKILL of the shared zygote, with two observed launcher hangs at N=10.
6. `-Xzygote` needs the research guest dex, a reflective `Daemons.start()`, and
   `-Xhidden-api-policy:disabled` on the zygote argv (F7); the guard `.so` is a Z4 rebuild with
   `-fno-stack-protector` (Z4 §6.5).
7. Per-session launcher cost (20.5–25.2 MiB Pss each; 479–485 MiB at N=20, ≈ 22 % of the total) is
   untouched by either arm — it is now the largest remaining per-session overhead.

**Risks (not measured here)**

1. **Security, untrusted**: F1 (proven reachable), F2/F3, F8/F9/F10 — excluded by ZS.
2. **Security, trusted multi-session**: F4/F5/F6/F7 first; the pre-fork invariant is review-only.
3. **Single point of failure**: a zygote crash, GC stall or OOM takes every session; the measured
   teardown cannot distinguish one hung child from a dead runtime.
4. **Compatibility**: proven only for `counter.apk`. Form/dialog/Gaten/React Native, repeated
   launch→teardown→relaunch, GC under sustained load, and per-session uid were not measured (Z4
   §6.7). If the §3 release is ART's post-fork compaction, it must be shown not to cost a real app
   its working set on the next allocation burst.
5. **Protocol**: ΣPss apportions shared pages as a single tree sees them; arm (c) also has a
   time-dependent steady state (§3). Percentages are sensitive to the launcher's cost, which neither
   optimisation touches, and this build's launchers are heavier (20.5–25.2 MiB) than Z4's (13.5 MiB),
   which lowers every saving percentage versus Z4's report.

---

## 8. Threshold verdicts (facts, whole system, same N)

| comparison | N=1 | N=5 | N=10 | N=20 |
|---|---:|---:|---:|---:|
| (c) vs (a), Z4 method (0.5 s) | −8.8 % | **+8.3 %** | +52.1 % | +56.8 % |
| (c) vs (a), steady state (60 s) | not run | **+41.0 %** | **+48.5 %** | not run (Z4-method run is already settled: +56.8 %) |
| (b) vs (a), Z4 method | −12.9 % | +6.2 % | +8.5 % | +9.1 % |
| (c) vs (a)+(b), Z4 method | +3.6 % | +2.3 % | +47.6 % | +52.4 % |
| (c) vs (a)+(b), steady state | — | +35.4 % | +42.1 % | — |
| marginal (ΣPss/session) | — | — | — | (c) 31.3 vs (a) 99.2 = **68.4 % less** |
| (b) marginal | — | — | — | 87.9 vs 99.2 = 11.4 % less |

1. **N=5: the 25 % threshold is met only under the steady-state protocol (+41.0 %) and not under
   Z4's 0.5 s protocol (+8.3 %).** The two readings differ by 33 points and the difference is
   entirely the zygote children's post-fork release (§3). Until the mechanism is identified, a
   reviewer should treat N=5 as *"between 8 % and 41 %"*, not as a point value.
2. **N=10: +48.5 % under the steady-state protocol and +52.1 % in the Z4-method run** (whose
   sample for the early children is already ≥10 s old, i.e. also settled) — met under both, agreeing
   to within 4 points. **N=20: +56.8 %** (Z4 method, settled by construction; no 60 s run at N=20).
3. **Versus (a)+(b) — the current path including B3's shared image — the zygote is 2–4 points lower
   than versus (a) alone at N=1/5 (because (b) itself saves 6–9 %) and 47.6 / 52.4 points higher at
   N=10/20.** Not met at N=5 under 0.5 s (+2.3 %); met at N=5 steady state (+35.4 %) and at N=10
   (+42.1 %) / N=20 (+52.4 %).
4. **N=1 fails in every arm** ((b) −12.9 %, (c) −8.8 %): both optimisations are per-extra-session
   wins, and a single-session workload pays for them.
5. **Correctness/isolation/pixels: no loss** in any arm at any N measured (§5). **Security: losses
   exist and are unchanged by Z5** (§6) — untrusted workloads are excluded by ZS independently of the
   memory numbers. **Compatibility: one fixture app only** (§7.4). By the gate's own wording
   ("without sacrificing correctness, security or compatibility"), the security column is not
   satisfied for untrusted workloads at any N, whatever the memory figure is.
6. The one metric that clears 25 % under *every* protocol is the marginal cost of one extra session
   for arm (c) (31.3–81.5 vs 99.2 MiB/session, −18 % to −68 %). The gate says whole-system, so this
   is a fact, not a pass.

---

## 9. Reproducing

```bash
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export CARGO_TARGET_DIR=/home/Qui/Projects/acos/.cache/z5-target
cargo build --release -p acos-cli -p acos-art        # main + the Z5 no-stub seam

# the three arms x N (one lock acquisition each, ~3 min per arm)
tools/art-runtime/run-exclusive.sh --task Z5 -- tools/guarded.sh --mem 24G --time 900 -- \
  results/art-runtime/evidence/Z5/run-z5.sh a              # optimised current
tools/art-runtime/run-exclusive.sh --task Z5 -- tools/guarded.sh --mem 24G --time 900 -- \
  results/art-runtime/evidence/Z5/run-z5.sh b              # + ACOS_SHARED_BOOT_IMAGE
tools/art-runtime/run-exclusive.sh --task Z5 -- tools/guarded.sh --mem 24G --time 900 -- \
  results/art-runtime/evidence/Z5/run-z5.sh c              # zygote + no-stub holder
# steady state (60 s settle) for arms (a) and (c)
tools/art-runtime/run-exclusive.sh --task Z5 -- tools/guarded.sh --mem 24G --time 840 -- \
  results/art-runtime/evidence/Z5/run-z5-settle60.sh
# tables
python3 results/art-runtime/evidence/Z5/tables.py
```

Evidence in git: `evidence/Z5/{run-z5.sh,run-z5-extra.sh,run-z5-settle60.sh,run-z5-c10s60.sh,
summarise.py,tables.py,summary.json,table.txt}` and the twelve+ per-configuration JSONs (`a-*`,
`a-*-r2`, `a-*-s60`, `b-*`, `c-*`, `c-*-s60`) — driver report, per-session replies, per-process
memory rows. Raw guest/launcher logs and the session run roots stay in `.cache/z5-run/` (not in git).
`CARGO_TARGET_DIR` was deleted after the report.
