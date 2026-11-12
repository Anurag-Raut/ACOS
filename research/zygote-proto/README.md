# Z1 — standalone Zygote-style ART prototype

Prototype only. See `results/art-runtime/Z1.md` for the measurements and the verdict.

* `java/dev/acos/z1/ZygoteMain.java` — the fork server: no-thread window, preload,
  `ZygoteHooks.gcAndFinalize()`, then `preFork()` → `fork()` → `postForkChild()`/`postForkCommon()`
  per child, plus one late fork after all children are reaped.
* `java/dev/acos/z1/Work.java` — the child's work (20 MiB, 5 full GCs, 4 contending threads,
  inherited/private statics) and the cold-start control (`main`, second arg `cold`).
* `java/dev/acos/z1/Nat.java`, `shim/z1fork.c` — the 91-line JNI fork shim.
* `java/dalvik/system/ZygoteHooks.java` — signature shadow of libcore's hidden class
  (never loaded; the boot class loader wins).
* `build.sh`, `run.sh`, `measure.py`, `evidence/` — build, the bwrap invocation copied from
  `crates/acos-art/src/lib.rs:1018-1240`, and the smaps harness.
