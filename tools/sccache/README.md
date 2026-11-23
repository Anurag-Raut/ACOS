# Shared compiler cache (sccache) for ACOS worktrees

`sccache` is the shared compiler cache every agent worktree uses. `tools/dev/agent-env.sh`
is the only place that wires it up; `docs/DEV_WORKFLOW.md` ("Shared compiler
cache") is the agent-facing description and `results/handoff/BUILDCACHE/README.md`
holds the measurements.

## What is installed

```
~/.cargo/bin/sccache        # user-local, no sudo, no system package
```

Stock [sccache v0.19.0](https://github.com/mozilla/sccache) **plus one patch**,
built by `tools/sccache/build.sh` from the release tarball whose sha256 the
script records and verifies:

| what | value |
|---|---|
| release tarball | `sccache-v0.19.0-x86_64-unknown-linux-musl.tar.gz` |
| tarball sha256 | `aa73c7270c997894038c0d8f0d297865f57569e1687883322bc431713d4bb9dd` |
| installed binary sha256 (2026-11-18) | `64a1b0e9327d99642a3a29ecfc14522448fff71d33b165a36ad7a4263b28b45a` |

The build uses `--no-default-features`, i.e. the local disk cache only. The
remote-storage backends (S3/GCS/Azure/…) pull in `opendal`, which requires a
newer rustc than the host toolchain (1.90.0), and ACOS does not use them.

## Why the patch (`sccache-basedirs-cwd.patch`)

`sccache`'s documented way to share one cache between checkouts of the same tree
is `SCCACHE_BASEDIRS`. For C/C++ it works: the preprocessor output is normalised
against the base directories before hashing. **For Rust it is a no-op.** The
Rust cache key hashes

* the compile **cwd** verbatim — and cargo sets that to the workspace root, so
  every worktree gets a different key for every crate; and
* every `CARGO_*` environment variable verbatim — including
  `CARGO_MANIFEST_DIR` (the crate's own directory) and `CARGO_TARGET_DIR`.

Measured before the patch: two identical crates in two directories, same
compiler, same flags, `SCCACHE_BASEDIRS` set → `Cache hits 0, Cache misses 2`.
So stock sccache gives a worktree reuse *within* itself and nothing *between*
worktrees, which is the case that matters here.

The patch (≈120 lines including comments, `src/compiler/rust.rs` only):

1. **cwd** — a cwd below a base directory hashes as `<basedir>` plus its path
   below the base directory with one leading component removed. That component
   is the interchangeable part: the worktree directory (`…/acos/t3-1a2b`) or,
   when the base directory is itself a checkout (`/home/Qui/Projects/acos`),
   nothing at all.
2. **`CARGO_*`** — a variable that rustc recorded in its dep-info as a real
   dependency of this compile is **skipped** here (it was already hashed
   verbatim a few lines above). Everything else is hashed through the same
   normalisation as (1), except `CARGO_TARGET_DIR`, which is dropped from the
   key for the same reason `--out-dir` is: it says where cargo puts artifacts,
   not what rustc emits.
3. `rust-version` in `Cargo.toml` 1.91.0 → 1.90.0 so the build is possible on
   the host toolchain (only needed for `--no-default-features`; the gated deps
   are the remote-storage ones that are not built).

### The correctness property this keeps

A crate whose artifact bakes in an absolute path is **never** served another
checkout's build: `env!("CARGO_MANIFEST_DIR")` (and any other `env!`/`option_env!`)
makes rustc write an `# env-dep:CARGO_MANIFEST_DIR=<abs path>` line into its
dep-info, sccache hashes those verbatim, and the patched `CARGO_*` loop skips
the same variable, so the key still contains the real path.

Verified (`results/handoff/BUILDCACHE/README.md` §Correctness): a crate using
`env!("CARGO_MANIFEST_DIR")` built in two directories stays at `hits 0 / misses 2`,
while an identical crate that does not use it goes `hits 1 / misses 0`.

## Rebuilding

```sh
tools/sccache/build.sh            # download (sha256-checked) → patch → build → install
KEEP_SRC=1 tools/sccache/build.sh # keep the build tree under ~/.cache
```

Then `sccache --version`, `sccache --show-stats`. The daemon keeps the
configuration it was started with, so after changing `SCCACHE_BASEDIRS` /
`SCCACHE_DIR` / `SCCACHE_CACHE_SIZE` run `sccache --stop-server` once; the next
build starts a server with the new settings (a stale server silently stops
producing cross-worktree hits). The script installs to
`~/.cargo/bin` (override with `SCCACHE_PREFIX`), never to `~/.local`.
