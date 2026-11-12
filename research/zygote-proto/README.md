# Z1/Z2 — standalone Zygote-style ART prototype

Prototype only. See `results/art-runtime/Z1.md` and `Z2.md` for the measurements and the verdict.

* `java/dev/acos/z1/ZygoteMain.java` — the fork server: no-thread window, preload,
  `ZygoteHooks.gcAndFinalize()`, then `preFork()` → `fork()` → `postForkChild()`/`postForkCommon()`
  per child, plus one late fork after all children are reaped.
* `java/dev/acos/z1/Work.java` — the child's work (20 MiB, 5 full GCs, 4 contending threads,
  inherited/private statics) and the cold-start control (`main`, second arg `cold`).
* `java/dev/acos/z1/Nat.java`, `shim/z2fork.c` — raw-syscall JNI shim (fork/wait plus the Z2
  sandbox: NEWUSER/NEWNS/NEWPID, tmpfs mounts, seccomp, capability drop).
* `java/dalvik/system/ZygoteHooks.java` — signature shadow of libcore's hidden class
  (never loaded; the boot class loader wins).
* `build.sh`, `run.sh`, `measure.py`, `z2-suite.sh`, `evidence/` — build, the bwrap invocation
  copied from `crates/acos-art/src/lib.rs:1018-1240`, the smaps harness, and the one-hold Z2
  suite (diagnostic + N=1/2/5).
