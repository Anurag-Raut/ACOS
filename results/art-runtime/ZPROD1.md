# ZPROD1 — the persistent zygote, and a reset that forks **one** child

Phase 1 of the production zygote (ADR-0026 decision 1). Design:
`docs/decisions/2026-11-14-zygote-prod.md`. This file is the measurement + evidence for the first
increment (P1) and the list of what is still open (P2–P6).

Status: **P1 mechanism proven; production wiring (`acos serve` owns the keeper) is P2.** Nothing
here is the default; every session in this report was started with the ZPROD1 seams named below.

---

## 0. What Z7 said, and what this increment changes

Z7 measured the zygote arms finishing **30.0/30.1 tasks/min vs 39.0/39.1** for the exec arms
(`results/art-runtime/Z7.md` §4) and named the cause itself: *"the zygote arms do fewer tasks per
minute because the prototype restarts the whole cohort to reset"* (§7 item 2: "a product zygote
that forks a new child per reset would not pay it; that configuration is **not measured**").

This increment measures exactly that configuration, for the first time, on real ART:

* the zygote is **persistent** — one process, one pre-fork core, alive across resets;
* a reset **kills one session's child and forks one fresh child** from that zygote;
* every sibling session keeps its process, its pid, its launcher and its app state;
* the reset reply proves freshness the same way the exec path does.

## 1. What changed (all gated; unset ⇒ the product path is unchanged)

| file | change |
|---|---|
| `research/zygote-proto/z3/java/dev/acos/art/ZygoteApp.java` | `z4Persist`: after the initial sessions, serve `spawn <tag>` / `kill <tag>` / `exit` from `<sessions>/cmd` (a FIFO) instead of forking a fixed cohort once; `z4ForkOne` is the per-session fork; `z4KillOne` ends one session. `writeText` now names a write failure instead of swallowing it. |
| `research/zygote-proto/shim/z2fork.c` | `Nat.setpgid0()` (syscall 109): the session's warden (A) becomes its own process-group leader, so `kill(-A, SIGKILL)` ends **that** session (A warden + B pidns init + C app) and nothing else. |
| `research/zygote-proto/java/dev/acos/z1/Nat.java` | the `setpgid0` declaration. |
| `crates/acos-art/src/launch.rs` | two gated lines: (1) forward `ACOS_ZYGOTE_PERSIST` into the zygote sandbox; (2) **a zygote host no longer carries the guest seccomp/Landlock preload** (`sb.seccomp_guest(session_dir.is_none() && zygote_app.is_none())`) and binds `/data/seccomp` for a zygote host too. Both expressions are exactly the old ones when `ACOS_ZYGOTE_APP` is unset. |
| `research/zygote-proto/z4/reset.py` | the driver for this increment (keeper + N foreign launchers + one single-session reset + the assertions in §3). |

### 1.1 The bug this found (worth knowing before P2)

With `ACOS_ZYGOTE_APP` set and no `ACOS_ZYGOTE_SESSION`, the launcher used to preload the guest
guard into the zygote itself. The guard's **Landlock** ruleset then denied the zygote's own writes
to its contract directory: `/sessions/zygote.pid: open failed: EACCES` (measured, kept in
`evidence/`). Z4 never hit it because its zygote host always *also* had a session directory, and
that path passed `seccomp_guest(false)`. The fix is the F1 rule applied one level up: **the guard
belongs to the app child, never to the pre-fork process** — a Landlock ruleset installed before the
fork is inherited by every child and cannot be widened.

## 2. Process tree as run

```text
driver (research/zygote-proto/z4/reset.py)
├── keeper launcher  acos-art launch (ACOS_ZYGOTE_APP, ACOS_ZYGOTE_PLAN_DIR, N=0, PERSIST=1)
│   └── bwrap → ZygoteApp (PID 2; pre-fork core built once in 13 ms)
│       ├── tag 0 child: A(z4ns) → B(z4init) → C(app)      [own mount+pid ns, own run root]
│       └── tag 1 child: same
└── session launcher k (foreign guest, ACOS_ZYGOTE_SESSION=<plan>/k)   ← child k's peer
```

## 3. The reset, measured (N=2, counter)

Evidence: `results/art-runtime/ZPROD1/reset-n2.json`, `zygote.log`, `session-{0,1}.log`.

| step | value |
|---|---|
| zygote boot (pre-fork core, once) | **13 ms** (`ZP1 ready … boot_ms=13`) |
| both sessions verified (`0` → tap → `1`) | yes, both; PNG md5 `9f3406c1…` after tap, `39d1e4e3…` before (the same bytes as Z3/Z4) |
| reset = `kill 0` → zygote ends the session's process group | **20.2 ms**, `status=0x9` (SIGKILL) on pid 8 |
| fresh launcher for the reset session (new peer, new contract) | **70.6 ms** (`respawn_ms`) |
| `spawn 0` → new child forked, sandboxed, first frame | included above; **total reset 390.1 ms** host-observed |
| child pid | **8 → 62** (a genuinely different process answered) |
| reset session's state | counter **`0`**; first frame md5 **`39d1e4e3…` = a brand-new session's first frame** (and ≠ the old child's post-tap `9f3406c1…`) |
| sibling session 1 | child pid **35 → 35**, counter **`1` → `1`**, its launcher never restarted |
| zygote | pid **2 → 2**, boot epoch **`26971873464` → `26971873464`** (the reset did **not** restart the cohort) |
| verdict | **PASS**, 0 failures (`reset-n2.json`), wall 7.76 s for the whole scenario incl. two launcher boots |

The freshness proof is the strongest form available and is *not* an ack: the reset child's first
frame is byte-identical to a never-tapped session's first frame, while the pre-reset frame of the
same session was the tapped one. That is what "no state leaked from the previous task" means
concretely on this engine.

## 4. What this does NOT show (honest list)

1. **No throughput or GiB-min number.** The Z7 loop was not re-run with this reset: `loop.py`'s
   reset is "close the cohort" by construction, and the engine-side keeper (P2) is what a product
   reset needs. §5 has the exact commands for the P2 measurement; no tasks/min or tasks/GiB-min is
   claimed here.
2. **Not wired into `acos serve`.** The keeper in this increment is driven by the research driver,
   not by `crates/acos-cli/src/engine`. So the flag-on gate/Gaten runs are **not** possible yet
   (they would be the exec path with an unused env var — that would be a false claim, so it is not
   made).
3. **No per-child cgroup** (F3): children still live in the keeper's scope, so one session's OOM
   would kill the cohort. The reset kills by **process group**, never by `cgroup.kill` (that is what
   Z7 §6 measured taking the zygote down). F3 is P2.
4. **No fd allowlist** (F4), no pre-fork invariant assert (F5), no `tgkill` argument filter (F2
   defence-in-depth): P3/P4. Siblings keep their per-child mount+pid namespaces (F1/F2 as built by
   Z4), which is what makes the single-child reset possible at all.
5. **The keeper is not idle-bounded** and does not survive a `serve` restart: P2/P6.

## 5. Reproducing

```bash
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_ART_BIN=$WT/.cache/g3cd-target/release/acos-art
export ACOS_CACHE=/home/Qui/Projects/acos/.cache
ZBUILD=zprod1-build ACOS_CACHE=$ACOS_CACHE bash research/zygote-proto/z3/build.sh

tools/art-runtime/run-exclusive.sh --task ZPROD1 -- tools/guarded.sh --mem 16G --time 300 -- \
  python3 research/zygote-proto/z4/reset.py --n 2 --reset 0 \
    --out .cache/zprod1/reset-n2 --json .cache/zprod1/reset-n2.json

# reset a sibling instead (same run shape, the other session stays up):
#   … reset.py --n 2 --reset 1
```
