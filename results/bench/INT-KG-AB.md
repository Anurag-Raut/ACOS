# Integration A/B: int-kg (K5 keystore + G3F1 first-frame wait) vs main 18a5329 — 2026-11-14

Interleaved, `acos-bench.sh --reps 1 --groups "a:latency:8G:300"`, `--exclusive`, 3 reps per arm, all rc=0, ok=true.
Main arm = main 18a5329 built in place (`.cache/wt-abmain3`, `cargo clean -p acos-art -p acos-cli` + rebuild;
`strings` shows only wt-abmain3 compile paths). Branch reps 134502/135014/135659, main 134122/134746/135332.
Medians of per-rep p50s; gate +5 % and +2 ms.

| metric | main | int-kg | Δ |
|---|---:|---:|---:|
| M4 launch strict | 306.67 | 306.38 | −0.1 % |
| M4 launch fast | 170.66 | 169.04 | −0.9 % |
| M5 screenshot | 24.40 | 24.42 | +0.1 % |
| M6 reset strict | 336.31 | 335.76 | −0.2 % |
| M6 reset fast | 189.92 | 196.07 | +3.2 % (+6.15 ms, under the 5 % gate) |
| M7 tree strict | 41.24 | 41.22 | −0.1 % |
| M7 tree fast | 0.40 | 0.53 | +0.13 ms (under the 2 ms gate) |
| M8 tap counter strict | 957.71 | 953.31 | −0.5 % |
| M8 tap counter fast | 44.76 | 45.21 | +1.0 % |
| M8 tap form strict | 943.92 | 942.32 | −0.2 % |
| M8 tap form fast | 45.39 | 44.34 | −2.3 % |
| teardown | 13.0 | 13.0 | 0 |

No regression. K5 moves the default guest uid to 10192 for every run; the A/B covers that default.

Methodology correction found during this A/B: the previous main arm binary (`acos-art`, copied from the
integration target) carried the integration worktree's compile-time guest-Java path, so its main arm compiled the
branch's guest Java (rep 1 of this A/B failed javac on `SoftwareKeyStore`). INT-PT-AB's main arm may therefore have
run TREE1's guest Java; TREE1's guest-side latency is re-checked separately (ABCHECK).

Correctness (`.cache/int/verify9.out`): acos-protocol 64, acos-apk 39, acos-wm 80, acos-cli 158, acos-services 331,
acos-verify 70 pass; acos-art 162/163 — `launch_hang::guest_exits_before_milestone_returns_exit_code_and_cleans_up`
timing bound (5 s) under load, see the flake note below. 15 gates (10 pass, 5 designed failures with their codes),
5 Gaten flows pass, 5 negatives ASSERTION_FAILED, GR4 4 pass + 4 negatives ASSERTION_FAILED. 0 false passes.

Flake note: `launch_hang::guest_exits_before_milestone…` 5 runs per arm right after the A/B — main 18a5329:
7.18 s FAIL, then 4×0.25 s; int-kg: 4.26 s, then 4×0.26–0.29 s. The slow first run after a build exists on main
too (cold launch-guest build/cache path); not attributable to this branch. Queued as LHFLAKE.
