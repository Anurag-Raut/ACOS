# ZM2 — host-side research harnesses

| file | what it is |
|---|---|
| `part1.py` | Imports Z4's driver and replaces its sampler: full `/proc/<pid>/smaps`, threads and fds for every `acos-art` launcher while N sessions are held. Part 1 of `results/art-runtime/ZM2.md`. |
| `drive_hostfork.py` | Prototype C driver: one warm host template (`ACOS_HOST_ZYGOTE_PLAN`) forks one launcher per session; each child gets its own socketpair stdio; the Z4 session/tap/isolation/md5 matrix is reused unchanged. |
| `sweep.sh` | Quick-win knob sweep (one N=1 session per allocator configuration + N=5 for the winning one). |
| `capture-alloc.c` | Allocator-only reproduction of the launcher's capture working set (three ~9.93 MiB frame buffers + a 1 MiB encoder chunk ring with cross-thread frees). `cc -O2 -o cap capture-alloc.c -lpthread`, then run with `none|trim|mmap|mmap_trim|arena|arena_trim|trim_thr` and read `/proc/<pid>/smaps_rollup` while it sleeps. |

All ART runs go through `tools/art-runtime/run-exclusive.sh --task … -- tools/guard/guarded.sh --mem 12G --time … -- …`;
scratch is under `/home/Qui/Projects/acos/.cache/zm2/`.

Env knobs added by this task (all unset by default):

* `ACOS_HOST_ZYGOTE_PLAN=<dir>` — prototype C template (see `crates/acos-art/src/host_zygote.rs`).
* `ACOS_ART_MALLOC_ARENA_MAX=N`, `ACOS_ART_MALLOC_MMAP_THRESHOLD_KB=N`,
  `ACOS_ART_MALLOC_TRIM_THRESHOLD_KB=N`, `ACOS_ART_TRIM_AFTER_CAPTURE=1` — W1 experiments.
