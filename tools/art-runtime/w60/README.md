# W60 death-attribution harness

`repro.py` launches N fresh ART sessions per rep through one `acos serve` and
monitors every descendant of the daemon (pid, ppid, pgrp, cgroup, lifetime) plus
a `kill(2)` logger preloaded into the daemon tree (`killwatch.c`).
`reset.py` does the same over `acos_reset` cycles. Both keep scratch.

Build the logger once:

    gcc -shared -fPIC -O2 -o .cache/w60/killwatch.so tools/art-runtime/w60/killwatch.c -ldl

Run (always under the host-wide lock and the guard):

    tools/art-runtime/run-exclusive.sh --task W60 -- \
      tools/guarded.sh --mem 8G --time 900 -- \
      python3 -I tools/art-runtime/w60/repro.py 0 12

    tools/art-runtime/run-exclusive.sh --task W60 -- \
      tools/guarded.sh --mem 8G --time 900 -- \
      python3 -I tools/art-runtime/w60/reset.py 0 3 20

Env: `W60_ACOS`, `W60_ART_BIN` (binary under test), `W60_BARE_SETTLE` (default 10 s,
matches `d2_warm.py`). Output: `.cache/w60/reps/`, `.cache/w60/resets/`.
The logger records the sender pid/comm and target of every SIGKILL, and every
`cgroup.kill` write, so a guest death is attributed to a caller or ruled out.
