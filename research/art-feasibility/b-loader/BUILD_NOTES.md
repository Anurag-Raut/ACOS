# b-loader — vendored loader patches (research only)

`vendor/acos-native-loader/` is a **copy** of `crates/acos-native-loader` with
the changes below. The real crate was not touched. Each change is marked with a
`B-LOADER PATCH` comment.

| file | patch |
|---|---|
| `src/loader.rs` | APS2 packed-relocation decoder rewritten to the real bionic format (`GROUPED_BY_INFO` / `GROUPED_BY_OFFSET_DELTA` / `GROUPED_BY_ADDEND` / `HAS_ADDEND`, offsets cumulative across the whole section, addend accumulator per group, `addend_in_place = !has_addend`). Reason: libart.so emits 8 groups with flags `0x0`; upstream aborts with "group flags 0x0 are unsupported". |
| `src/loader.rs`, `src/elf.rs` | `R_X86_64_TLSGD` (19) / `TLSLD` (20) applied as GOT-pair `{module_id, offset}` writes, consumed by the existing `__tls_get_addr`; `TPOFF32` (23) / `GOTTPOFF` (22) rejected explicitly. 376 such relocations in libart/libartbase. |
| `src/loader.rs` | `is_shim_system_lib` also matches `ld-android.so`: on Android 14 it is a 2-byte `ud2` stub and every `__loader_*` symbol points at the trap; mapping it shadows the bionic table (`resolve_global` prefers loaded objects) and ART dies with SIGILL right after libnativeloader init. |
| `src/loader.rs` | `pub fn loaded_objects()` — `(name, map_base, map_len)` for crash-address triage. |
| `src/bionic/mod.rs` | `art_gaps::table()`, `art_gaps::path_table()`, `art_gaps::namespace_table()` installed last (so they override plain forwards). |
| `src/bionic/dl.rs` | `dlsym(RTLD_NEXT)` now resolves from the bionic table (libsigchain resolves its predecessor chain this way). `AEXP_TRACE_DL=1` traces dlopen/dlsym failures. |
| `src/bionic/art_gaps.rs` (new) | 166 bionic/NDK entries ART needs: 87 host forwards (libc, `libgcc_s.so.1` `_Unwind_*`, `libz.so.1` inflate/deflate/adler32), 79 shims (`*64` signal calls, `android_fdsan_*`, `android_get_device_api_level`, liblog event API, fortify `__*_chk`, `atrace_*`, `ashmem_*`, `_ctype_`, `getprogname`, properties), a generated 146-entry bionic→glibc `sysconf` map, an Android-absolute-path remap (`/system`, `/apex`, … → sysroot, plus per-path overrides), and a minimal `__loader_*` namespace model. |

Reproduce:

```
CARGO_TARGET_DIR=.cache/target-artexp-b CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=2 \
  cargo build --manifest-path research/art-feasibility/b-loader/Cargo.toml --release
.cache/target-artexp-b/release/gapscan --root .cache/art-exp/sysroot --entry libart.so
.cache/target-artexp-b/release/artspike --step load            # maps + relocates + ctors
.cache/target-artexp-b/release/artspike --step load --confine  # same, sandboxed
.cache/target-artexp-b/release/artspike --step create --noimage
```

`artspike` also supports `--step run --dex <classes.dex> --class LMain;`,
`--natives` (RegisterNatives of a Rust function), `--xtrace` (loader report
JSON), `--lazy`, `--image <boot.art>`, `--mem-gib N`, `--option -X...`.
Env: `AEXP_TRACE_MAPS`, `AEXP_TRACE_DL`, `AEXP_TRACE_RELOC`.
