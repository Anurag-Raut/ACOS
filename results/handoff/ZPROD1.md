# ZPROD1 handoff — the production zygote, Phase 1

Branch `zygote-prod-1` off `main` `863def1`. Design: `docs/decisions/2026-11-14-zygote-prod.md`.
Measurement + evidence: `results/art-runtime/ZPROD1.md`, `results/art-runtime/ZPROD1/`,
`EXPERIMENTS.md` §E2.17.

## What is done (P1)

* The design (which zygote, process tree, ownership, per-session reset, F1–F4 answers, failure
  handling, the flag, P1–P6 with acceptance).
* The **persistent zygote** and the **single-child reset**, proven at N=2 on real ART: reset kills
  one session's process group, forks one fresh child from the same zygote (same pid, same boot
  epoch), the reset session's state is provably fresh (counter `0`, first frame byte-identical to a
  brand-new session), and the sibling keeps its process, pid and state.
* Two real defects found and fixed: the zygote host must not carry the guest Landlock/seccomp guard
  (it denied the zygote's own writes with `EACCES`), and `writeText` now reports failures.

## What is NOT done (and is not claimed)

* `acos serve` does not own a keeper yet, so **`ACOS_ART_ZYGOTE=on` is not a usable product knob**.
  The increment is driven by `research/zygote-proto/z4/reset.py`.
* Consequently: no flag-on gate/Gaten run, no crate test for the reset path, no throughput or
  GiB-min number, no per-child cgroup (F3), no fd allowlist (F4), no pre-fork invariant assert
  (F5), no `tgkill` argument filter (F2 defence-in-depth).
* The flag-off product path is unchanged **by construction** (every new line is inside
  `ACOS_ZYGOTE_APP`/`ACOS_ZYGOTE_PLAN_DIR`/`ACOS_ZYGOTE_PERSIST` gates) and the flag-off gates in
  §"Next" confirm it.

## P2 — the next increment (smallest useful slice, ~1 task)

**Goal:** `acos serve` owns a keeper per (APK, boot image, geometry, insets, animations); a session
reset goes through it; the reply carries `state_reset`/`data_cleared` + `child_pid`,
`previous_child_pid`, `run_root`, `zygote_boot_epoch`.

Work items, in order:

1. `crates/acos-art/src/zygote_prod.rs`: `PersistentZygote` = keeper launcher + plan dir + FIFO +
   `spawn(tag, session_dir)`, `kill(tag)`, `boot_epoch()`, `reset(tag, new_session_dir)`,
   `shutdown()`. Port of `research/zygote-proto/z4/reset.py` steps 1–4, file-based acks
   (`<plan>/<tag>/{spawned,reaped}`, `<plan>/ready`).
2. `crates/acos-cli/src/engine/{serve,art}.rs`: one keeper map keyed by
   `(apk_sha, boot_image_mode, width, height, density, insets, animations)`; `ArtSession` gets the
   keeper handle; `reset()` uses `keeper.reset(tag, new_dir)` instead of
   `shutdown + start_with_options`; `status_value`/`reset` replies gain the fields above; flag off
   ⇒ the current code path verbatim (assert the argv/call equality in a test).
3. `docs/SECURITY.md`: the `ACOS_ART_ZYGOTE*` rows (F12).
4. Tests (Rust, `crates/acos-cli/tests/`): (i) two sessions, reset A → A's counter `0`, A's
   `child_pid` changed, B's pid and counter unchanged, `zygote_boot_epoch` unchanged; (ii) kill the
   zygote → `ZYGOTE_DEAD` error + new epoch + both sessions reforked fresh; (iii) flag off → the
   spawn argv recorded by a fake `acos-art` is byte-identical to `main`'s.

**Acceptance for P2:** crate tests above pass; flag-off gates unchanged (same 11 pass / 3 fail list
as P1); **flag-on**: `v2-art-counter` + one Gaten flow (`scenarios/gaten-home.yaml`) pass; F3 landed
with the two-phase start gate and an N=2 OOM test proving one session's OOM kills only itself.

Then **P3** (F4/F5), **P4** (F2 filter), **P5** (the owner gate) per the design doc.

## Exact commands

```bash
WT=/home/Qui/.paseo/worktrees/3oxa6xao/g3cd
cd $WT
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_ART_BIN=$WT/.cache/g3cd-target/release/acos-art
export ACOS_CACHE=/home/Qui/Projects/acos/.cache
CARGO_TARGET_DIR=$WT/.cache/g3cd-target tools/guarded.sh --mem 12G --time 900 -- \
  cargo build --release --offline -p acos-art -p acos-cli
ZBUILD=zprod1-build ACOS_CACHE=$ACOS_CACHE bash research/zygote-proto/z3/build.sh

# the P1 proof (N=2, reset tag 0; use --reset 1 for the sibling)
tools/art-runtime/run-exclusive.sh --task ZPROD1 -- tools/guarded.sh --mem 16G --time 300 -- \
  python3 research/zygote-proto/z4/reset.py --n 2 --reset 0 \
    --out .cache/zprod1/reset-n2 --json .cache/zprod1/reset-n2.json

# flag-off gates (byte-identical product path)
cp /home/Qui/Projects/acos/.cache/wt-int/.cache/int/run_suite.sh .cache/zprod1/   # then edit
#   the target/output paths inside it, and run the 11+3 gate list from the task brief.

# crate tests
CARGO_TARGET_DIR=$WT/.cache/g3cd-target tools/guarded.sh --mem 16G --time 1200 -- \
  cargo test --release --offline -p acos-art -- --test-threads=1 \
    --skip refuses_to_run_when_no_cgroup_can_be_created
CARGO_TARGET_DIR=$WT/.cache/g3cd-target tools/guarded.sh --mem 16G --time 600 -- \
  cargo test --release --offline -p acos-cli

# P5, the owner gate (after P2): Z7's loop with the keeper in the denominator, ≥3 interleaved reps
#   ACOS_ART_ZYGOTE=on  vs  ACOS_ART_BOOT_IMAGE=shared-fixed (the harder exec arm)
tools/art-runtime/run-exclusive.sh --task ZPROD1-loop -- tools/guarded.sh --mem 24G --time 700 -- \
  bash results/art-runtime/evidence/Z7/run-loop.sh c cp    # replace with the keeper arm in P2
```

## Open decisions for the orchestrator

1. **P2's cgroup mechanism** (F3): the only unprivileged option is a host-side scope per child plus
   a start-gate byte from the child before it runs app code. It costs ~1 ms per fork and one
   protocol step. Approve, or accept the shared scope for now and defer F3 to P3.
2. **The idle policy**: a keeper that lives 30 s after its last session (design default) or one that
   lives until the serve ends. The first costs a zygote boot on a bursty loop, the second holds
   ~40 MiB Pss.
3. **Whether P5's comparison arm may use `shared-fixed`** (the exec path's best memory arm, which
   the zygote path cannot combine with `-Xzygote`, F11). The design says yes — it is the honest
   denominator — but it makes the 25 % gate harder than Z7's.
