# b-loader — Approach B spike: real ART's libart.so under the ACOS loader

Isolated feasibility experiment. **Does not modify any ACOS source.** Path
dependencies into `crates/` are read-only.

* `gapscan` — inventories every `.so` in an extracted Android 14 x86_64
  sysroot, walks the `DT_NEEDED` closure from `libart.so`, and classifies each
  undefined import against `acos_native_loader::bionic::resolve`.
* `artspike` — maps `libart.so` with `acos_native_loader::Loader`, resolves
  `JNI_CreateJavaVM`, calls it, and (if a VM comes up) runs a DEX `main`.

Scratch/output policy for this experiment: `CARGO_TARGET_DIR=.cache/target-artexp-b`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_BUILD_JOBS=2`, everything through
`tools/guarded.sh`.
