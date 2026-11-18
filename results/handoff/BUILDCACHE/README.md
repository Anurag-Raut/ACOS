# BUILDCACHE — shared Rust compiler cache for agent worktrees

**Status: done.** Every agent worktree on this host now shares one sccache
cache, wired in exactly one place (`tools/agent-env.sh`, documented in
`docs/DEV_WORKFLOW.md` §14). Cold-vs-warm and second-worktree numbers below,
plus the correctness checks for the two ways a shared cache can serve a stale
artifact (changed source, build fingerprint).

Branch: `buildcache` (from `modules` @ `aaf518bf8`).

## 1. What is installed

| item | value |
|---|---|
| wrapper | `~/.cargo/bin/sccache` — sccache v0.19.0 + `tools/sccache/sccache-basedirs-cwd.patch` |
| wrapper sha256 | `64a1b0e9327d99642a3a29ecfc14522448fff71d33b165a36ad7a4263b28b45a` |
| release tarball | `sccache-v0.19.0-x86_64-unknown-linux-musl.tar.gz`, sha256 `aa73c7270c997894038c0d8f0d297865f57569e1687883322bc431713d4bb9dd` (verified by `tools/sccache/build.sh`) |
| cache dir | `/home/Qui/.cache/sccache` (disk, LRU) |
| size cap | `SCCACHE_CACHE_SIZE=40G` (current size after the runs below: **337 MB**) |
| base dirs | `SCCACHE_BASEDIRS=/home/Qui/.t3/worktrees/acos:/home/Qui/Projects/acos/.cache:/home/Qui/Projects/acos` |
| install path | user-local (`~/.cargo/bin`), **no sudo**, nothing written to `~/.local` |

Nothing is set in `~/.cargo/config.toml` or any global file: another project on
this host does not see `RUSTC_WRAPPER` unless it sources the ACOS env script.

### Why stock sccache was not enough (the one patch)

`sccache`'s documented `SCCACHE_BASEDIRS` ("share one cache between checkouts of
the same tree") is implemented for C/C++ only: the Rust key hashes the compile
**cwd** verbatim (cargo sets it to the workspace root) and every `CARGO_*`
variable verbatim (`CARGO_MANIFEST_DIR` is the crate's own directory), so two
worktrees of the same sources never share an entry.

Measured with **stock** sccache 0.19.0 before patching — identical crate, two
directories, same compiler and flags, `SCCACHE_BASEDIRS` set:

```
Compile requests executed 1   Cache hits (Rust) 0   Cache misses (Rust) 1   Base directories /home/Qui/.cache/sccache-test3/
```

Same test after the patch (`tools/sccache/sccache-basedirs-cwd.patch`):

```
Compile requests executed 1   Cache hits (Rust) 1   Cache misses (Rust) 0
```

The patch normalises the cwd and the non-dependency `CARGO_*` values against the
base directories (one leading path component below a base dir — the worktree
directory — is treated as interchangeable) and **keeps every variable rustc
recorded as a real dependency hashed verbatim**, so a crate that bakes an
absolute path into its artifact is still per-checkout. `tools/sccache/README.md`
has the full rationale; `tools/sccache/build.sh` reproduces the binary.

## 2. Measurements

Method: fresh `CARGO_TARGET_DIR`, `sccache --zero-stats` before each run, then

```
source tools/agent-env.sh            # RUSTC_WRAPPER, SCCACHE_*, debug=0, incremental=0
tools/guarded.sh --mem 12G --time 900 -- cargo test -p acos-art --no-run --offline
```

`-p acos-art --no-run` = **184 compile requests / 124 crates** (the acos-art
dependency closure plus its test binaries). Logs: `.cache/bc-all.log`
(`bc-run.sh` per phase, kept in the worktree's `.cache/`, not committed).

| run | worktree / branch | cache state | wall | Rust hits | Rust misses | hit rate |
|---|---|---|---|---|---|---|
| **cold** | this worktree, `buildcache` | cache emptied (first agent on the host) | **12.4 s** | 0 | 122 | 0 % |
| **warm** | same worktree, target wiped | same cache | **4.5 s** | 122 | 0 | **100 %** |
| **second worktree** | `/home/Qui/.t3/worktrees/acos/bc-wt2`, branch `bc-wt2` (`relnotes040` + this branch's commit — a *different* branch) | same cache | **10.3 s** | 101 | 21 | **82.8 %** |

Raw lines:

```
TIMING phase=cold label=cold rc=0 wall_s=12.4 host_cpu_s=399.1
Cache misses (C/C++) 60   Cache misses (Rust) 122   Cache hits rate 0.00 %
TIMING phase=warm label=warm rc=0 wall_s=4.5  host_cpu_s=143.9
Cache hits (C/C++) 60     Cache hits (Rust) 122     Cache hits rate 100.00 %
TIMING phase=wt2  label=wt2  rc=0 wall_s=10.3 host_cpu_s=330.0
Cache hits (Rust) 101     Cache misses (Rust) 21    Cache hits rate (Rust) 82.79 %
```

* **2.8x** wall-time win for the same worktree, **~1.2x** for a second worktree
  of a different branch (it reuses 101 of 122 crates; the 21 misses are the
  build.rs / `env!`-dependent crates, see §3).
* `host_cpu_s` is the `/proc/stat` delta for the whole host: other agents were
  building during these runs (6–7 concurrent `rustc`/`cargo` processes were
  counted before the warm and wt2 runs), so the CPU column is host load, not
  this build's CPU. **Wall time is the number to trust.** The cold run had the
  same caveat; it is if anything pessimistic (it shared 32 cores with other
  agents).
* These numbers are for the `acos-art` closure, which is what the brief asked
  for. The same cache covers the whole workspace (`cargo build --workspace`):
  a crate is keyed by its sources and flags, not by the `-p` selection, so the
  first agent pays the cold cost once for every crate it touches and every
  later worktree — on any branch — pays only for what it changed.
* Target-dir size with `debug=0` + `incremental=0`: `.cache/buildcache-target`
  **564 MB** for this closure (vs multi-GB with debug info), and the whole
  337 MB cache is smaller than one such target dir.

## 3. Correctness — no stale artifacts

**(a) Changed source is never served from the cache.** Appending two lines to
`crates/acos-art/src/lib.rs` and rebuilding `-p acos-art -p acos-mcp`:

```
Cache hits (Rust) 8   Cache misses (Rust) 12        # the edited crate + its dependents recompile
probe_in_acos_art_rlib=2                            # the new string is in the new rlib
```

Reverting the edit and rebuilding returns to cache hits for the unchanged
crates (`Cache hits (Rust) 9 / Cache misses (Rust) 4`; the 4 are the edited
crate's own bin/dependents, which are not cacheable or changed mtime only).

**(b) The `acos-mcp` build fingerprint is not shared across checkouts.** The
fingerprint is `crate version + git sha + FNV hash of the workspace sources`
(`crates/acos-mcp/build.rs`), passed to rustc as `cargo:rustc-env` and baked in
with `env!`. Two checks:

```
# one source edit changes the baked hash and forces a miss
--- fingerprint hex literals that changed after a source edit ---
> 1b55ca27e147f2a1
< afe8c5bd2ac1d0c4
stats_after_edit: Cache hits (Rust) 0   Cache misses (Rust) 1

# a second worktree embeds its own sha, never this checkout's
wt_sha_in_before=1        wt2_sha_in_before=0        # this worktree's binary
wt2_sha_in_wt2_binary=1   wt_sha_in_wt2_binary=0     # the other worktree's binary
```

This holds because of how the patch is written: rustc records
`# env-dep:CARGO_MANIFEST_DIR=…` (and every other `env!`/`option_env!` value) in
its dep-info, sccache hashes those verbatim, and the patched `CARGO_*` loop
*skips* any variable that appears there. A crate that bakes an absolute path is
therefore recompiled per checkout — which is why 21 crates miss in the second
worktree while 101 hit.

Unit-level check of the same property (two directories, `SCCACHE_BASEDIRS` set):

```
crate with `env!("CARGO_MANIFEST_DIR")`  -> Cache hits (Rust) 0  Cache misses (Rust) 1   (per-checkout, correct)
identical crate without it               -> Cache hits (Rust) 1  Cache misses (Rust) 0   (shared)
```

## 4. How an agent uses it

```bash
source tools/agent-env.sh        # or: echo 'source tools/agent-env.sh' > .cache/env.sh
cargo test -p acos-art --no-run
sccache --show-stats             # hits, misses, cache size
```

`docs/DEV_WORKFLOW.md` §14 is the reference; `tools/sccache/README.md` explains
the wrapper and how to rebuild it. `CARGO_TARGET_DIR` is still per-track (the
brief's rule) — the cache, not a shared target dir, is what makes that cheap.

## 5. Evidence files

| file | what |
|---|---|
| `results/handoff/BUILDCACHE/README.md` | this report |
| `tools/agent-env.sh` | the single env entry point |
| `tools/sccache/build.sh` | reproducible build/install of the wrapper |
| `tools/sccache/sccache-basedirs-cwd.patch` | the one patch, with its rationale |
| `tools/sccache/README.md` | why the patch exists, what it guarantees |
| `docs/DEV_WORKFLOW.md` §14 | agent-facing documentation |
