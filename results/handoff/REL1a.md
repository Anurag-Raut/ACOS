# REL1a handoff — installed ACOS (`ACOS_HOME`, `tools/release/install.sh`)

**Worktree:** `/home/Qui/.paseo/worktrees/3oxa6xao/h4`
**Branch:** `rel1a-install-layout` (from `main` = `389429a`)
**Commits:** `eb4e266` (code) + the commit carrying this file (docs, `results/rel1a/`).
**Date:** 2026-11-14. Not pushed, not merged.
**Track:** REL1a. REL1b (in-process host lock, MCP registration, agent guide) is a
different branch; `crates/acos-mcp` and `docs/MCP.md` were not touched here.

---

## 1. What changed

One rule replaces the "run from inside the repo" requirement:

```
root := $ACOS_HOME  (when set, non-empty)
      | else $ACOS_ROOT, else nearest ancestor of exe/cwd with crates/ + Cargo.toml
```

Every runtime asset and cache path is already `root.join(<rel>)`, so the install
root uses the *same relative layout* as the checkout and the dev path is
byte-identical when `ACOS_HOME` is unset (`acos_art::repo_root_with(None)` still
returns the checkout — asserted by a unit test).

| file | change |
| --- | --- |
| `crates/acos-art/src/lib.rs` | `acos_home()`, `dev_root()`, `repo_root_with(Option<PathBuf>)`, `asset_path`/`asset_path_with` (home → checkout → compiled-in), `compiled_root()`; `Error::NoRepoRoot` message now names `ACOS_HOME`. 4 new unit tests. |
| `crates/acos-art/src/app.rs` | `GUEST_SOURCE` stays as the compiled-in fallback; new `guest_source_dir()` resolves at run time. `render_guest_sources`, `build_guest` use it. |
| `crates/acos-art/src/session.rs`, `launch.rs` | the other two `GUEST_SOURCE` users (`session_guest_sources`, `build_launch_guest`). |
| `crates/acos-art/src/boot_image.rs` | boot-image cache: `ACOS_HOME/.cache/acos` inserted after the explicit overrides, before `XDG`/`HOME`. |
| `crates/acos-art/src/main.rs` | the `verify` corpus paths go through `asset_path` (dev fixtures; no install-time copy). |
| `crates/acos-art/src/build_cache.rs` | doc only (root is `ACOS_HOME` when set). |
| `crates/acos-cli/src/root.rs` | **new**: `acos_home()` + `runtime_root(_with)` (same rule; `acos-cli` does not link `acos-art`). 2 unit tests. |
| `crates/acos-cli/src/prewarm.rs` | local `repo_root()` deleted in favour of `crate::root::runtime_root()`; shared-boot-image cache gets the `ACOS_HOME` arm. |
| `crates/acos-cli/src/native_launch.rs` | native session root is `<root>/.cache/sessions/w59a-<pid>` (was a **cwd-relative** `.cache/sessions/...`, i.e. it wrote into whatever directory you ran from); `boot_root()` looks in `<root>/.cache/boot/android-34`. |
| `crates/acos-cli/src/doctor.rs` | installed-layout checks, `bwrap`/`systemd-run`, NDK clang, runtime-vs-fixtures labels for the SDK/JDK tools; `--json` subject gains `layout` and `acos_home`. |
| `crates/acos-runtime/src/boot_image_cache.rs` | `ACOS_HOME` arm in the cache chain. |
| `crates/acos-services/src/lib.rs` | `workspace_path` consults `$ACOS_HOME` first (`parcelable` uses it at run time). |
| `crates/acos-services/src/keychars.rs` | `keychars_dir` consults `$ACOS_HOME/.cache/art/sysroot` before the compile-time checkout. |
| `tools/release/install.sh` | **new**: build (or `--from-target`), copy, wrappers, `current`, idempotency, size. |
| `docs/INSTALL.md` | **new**: install, layout, state, env vars, upgrade, uninstall, runtime-vs-fixtures tool list. |

`tools/release/install.sh` copies the trees from their **resolved** path (the
worktree's `.cache/art` and `.cache/boot` are symlinks into the main checkout)
while preserving symlinks *inside* them (the sysroot's own `libc++.so`-style
links are partly dangling in the source extraction, so `cp -L` is not an
option), then fails closed if any link resolves outside the install root.

## 2. Acceptance

| # | check | result | evidence |
| --- | --- | --- | --- |
| 1 | path-resolution unit tests | **PASS** | `acos_art::tests::{acos_home_overrides_every_root_lookup, unset_home_resolves_to_the_checkout_unchanged, guest_asset_resolves_home_then_checkout_then_compiled_path, compiled_root_is_the_checkout}` + `acos_cli::root::tests::{acos_home_wins, unset_resolves_to_the_checkout}` (in the `-p acos-art` / `-p acos-cli` runs below) |
| 2 | install + run from outside a repo | **PASS** | `results/rel1a/doctor-installed.txt`, `.cache/rel1a/evidence/{counter,gaten,dev/*}/verdict.json`, `results/rel1a/prefix-files.txt` |
| 3 | dev path unchanged | **PASS** | the four scenario runs (`ACOS_HOME` unset) + `cargo test -p acos-art` 118 passed / `-p acos-cli` all green |
| 4 | docs + handoff | this file + `docs/INSTALL.md` |

### 2.1 Install

```
tools/release/install.sh --from-target .cache/h4-target \
  --prefix <WT>/.cache/rel1a/prefix --version 0.2.0
→ installed ACOS 0.2.0, size 1.5G (3134 files in MANIFEST.txt)
→ <prefix>/bin/{acos,acos-mcp} wrappers, share/acos/current -> 0.2.0
```

Idempotent rerun: `updating existing ... in place (idempotent)`, size 1.7G (the
first run's state, `.cache/acos` 173 MB, is kept). Refuse path verified: a
pre-existing `share/acos/0.3.0-other` with no `VERSION` marker →
`exists and was not installed for version 0.3.0-other; pass --force`.

Installed tree: 3273 files, 276 symlinks, listing kept in
`results/rel1a/prefix-files.txt` / `results/rel1a/prefix-symlinks.txt`. The
prefix was **deleted** after acceptance (disk).

### 2.2 From `/home/Qui/Projects/acos/.cache/rel1a-outside` (empty dir)

`acos inspect /home/Qui/Projects/acos/fixtures/build/apps/counter.apk` →
exit 0, package `dev.acos.fixtures.counter`, 1 activity, `classes.dex` 4684 B,
v1/v2/v3 signatures. The directory was still empty afterwards (no cwd writes).

`acos doctor` → `acos doctor: healthy (0 failure(s))`, exit 0
(`results/rel1a/doctor-installed.txt`): `acos_home`, `install_version`,
`install_sysroot`, `install_fonts`, `install_overlays`, `install_bootclasspath`,
`install_guest_sources`, `install_acos_art`, `install_state` all `ok`, plus
SDK/JDK/NDK/bwrap/systemd-run ok.

`acos verify --engine art` through
`/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task REL1a -- tools/guarded.sh --mem 10G --time 600 --`:

| scenario (copy under `.cache/rel1a/scenarios/`, `app:` an absolute repo path) | verdict | steps |
| --- | --- | --- |
| `v2-art-counter.yaml` | **pass** (engine art, fidelity art-reference) | 11, 0 unsupported |
| `gaten-home.yaml` | **pass** (engine art, fidelity art-reference) | 14, 0 unsupported |

`gaten-home` needs `--engine art`: its `animations: off` is an ART-only setting
and `native-preferred` refuses it with `UNSUPPORTED_BACKEND` (a real refusal, not
a pass). Recorded here so the next agent does not re-derive it.

### 2.3 Nothing written into the checkout

Stamp taken before the outside-repo runs, then
`find /home/Qui/Projects/acos -newer <stamp> -not -path '*/.cache/art-locks/*' -not -path '*/.paseo/*'`.

| old write target | newer entries |
| --- | --- |
| `.cache/art/data` | 0 |
| `.cache/art-sock` | 0 |
| `.cache/sessions` | 0 |
| `.cache/acos` | 0 |
| `.cache/boot/android-34` | 0 |
| `.cache/art/sysroot` | 0 |
| `fixtures/build`, `out` | 0 |
| `~/.cache/acos` (HOME boot cache) | 0 |
| `.cache/art/build-cache` | **2 new keys — not mine** |

The two new build-cache keys (`46a7c2c1…`, `ab761369…`, 18:34:45 / 18:35:57) do
**not** exist in the installed root's cache, which holds the 7 keys my runs
created (18:35); the same minutes are when the concurrent `.cache/wt-int` agent
was writing `out/gaten-lesson/*` into the main checkout. Every other old target
is untouched, the outside directory stayed empty, and the installed root grew
instead: `.cache/art` 860 MB, `.cache/boot` 328 MB, `.cache/acos` (boot image)
173 MB. The repo-wide `find` also lists `results/tmp/guarded.log` — the lock
script's shared log, written by any guarded run, including concurrent ones.

Honest limit: the named outside directory is *inside* the repo directory but
outside any checkout, so `doctor`'s workspace probe can still find the checkout
(the `workspace` check reported `/home/Qui/Projects/acos`). The installed arm of
that check is covered by `install_*` + the `layout` / `acos_home` subject fields,
not by this run.

### 2.4 Dev path (`ACOS_HOME` unset, `ACOS_FONT_DIR` set, repo binaries)

| gate | verdict | steps |
| --- | --- | --- |
| `v2-art-counter` | pass | 11 |
| `w22-counter-reset` | pass | 9 |
| `form-validation` | pass | 28 |
| `gaten-home` | pass | 14 |

```
cargo test --release --offline -p acos-art -- --test-threads=1 \
  --skip refuses_to_run_when_no_cgroup_can_be_created     → 118 passed, 0 failed (+ 16 integration suites, 0 failed)
cargo test --release --offline -p acos-art --test sandbox_s5 -- --test-threads=1 \
  --exact refuses_to_run_when_no_cgroup_can_be_created    → 1 passed
cargo test --release --offline -p acos-cli                → all suites ok (92 + 6 + 4 + 1 + 2 + 7 + 1 + 11 + …)
```

(The `--exact` test needs `--test sandbox_s5`: the bare `--exact <name>` filters
every binary to 0 tests, which "passes" without running anything.)

## 3. Runtime paths found, and where they resolve now

| path (as the code names it) | who reads/writes it | with `ACOS_HOME` |
| --- | --- | --- |
| `.cache/art/sysroot`, `.cache/art-exp/sysroot` | `ArtRoot::resolve`, prewarm | `$ACOS_HOME/...` | 
| `.cache/art/data` | `ArtRoot::resolve`, prewarm | `$ACOS_HOME/...` (state) |
| `.cache/boot/android-34/{fonts,overlays,BOOTCLASSPATH}` | `app.rs` fonts/overlays, `lib.rs` boot classpath, doctor, native boot root | `$ACOS_HOME/...` |
| `research/art-feasibility/a-dalvikvm/bootclasspath.txt` | `lib.rs` legacy classpath | `$ACOS_HOME/...` (copied by the installer) |
| `crates/acos-art/guest/java` (**compile-time**) | `app.rs`, `session.rs`, `launch.rs` | runtime lookup: `$ACOS_HOME/...` → checkout → compiled-in |
| `.cache/art/build-cache` (`ACOS_ART_BUILD_CACHE`) | `build_cache.rs` | `$ACOS_HOME/...` |
| `.cache/acos-art-build-cache` | `build_cache.rs` fallback when no root | unchanged (install always has a root) |
| `.cache/art-sock` | `launch.rs` `ShortSocketDir` fallback | `$ACOS_HOME/...` |
| `.cache/sessions` | `acos-cli` native engine (was **cwd-relative**) | `$ACOS_HOME/...` |
| `.cache/acos` / `XDG_CACHE_HOME` / `~/.cache/acos` → `art/boot-images` | `boot_image.rs`, `prewarm.rs`, `acos-runtime` boot cache | `$ACOS_HOME/.cache/acos/art/boot-images` |
| `.cache/boot/android-34` (native `/system`) | `native_launch.rs` | `$ACOS_HOME/...`, then `target/tmp/...` |
| sysroot `system/usr/keychars` | `acos-services::keychars` | `$ACOS_HOME/.cache/art/sysroot/...` |
| `.cache/boot/android-34/dex/framework` | `acos-services::workspace_path` (parcelable) | `$ACOS_HOME/...` |
| `target/{debug,release}/acos-art` | `engine/art.rs::resolve_binary` | unchanged (the install puts `acos-art` next to `acos` in `bin/`, which is the same rule) |
| `fixtures/build/dexprogs`, `fixtures/expected-art/dexprogs` | `acos-art verify` corpus | `asset_path`: `$ACOS_HOME/...` → checkout (dev-only corpus, not installed) |
| `fixtures/reference-device`, `tools/apk_append.py`, `fixtures/build/apps/debug.keystore` | `acos-cli parity`, doctor workspace | **checkout only** (dev tools; doctor now says so in an install) |

Embedded (no file needed at run time): `seccomp_guest.c`, `shim/artshim.c`,
`shim/render_shim.c` — all `include_str!`.

## 4. Remains / decisions

* **`crates/acos-mcp`** still resolves the boot dir through `~/.cache/acos/boot/android-34`
  fallbacks (`rn_backend.rs`, `session_backend.rs`) and `CARGO_MANIFEST_DIR`
  hints (`flutter_backend.rs`). Left alone on purpose (REL1b owns that crate);
  `ACOS_BOOT_DIR` and `ACOS_HOME` handling there is the next small step.
* **`acos-cli/src/parity.rs`** resolves `fixtures/reference-device` from
  `CARGO_MANIFEST_DIR` (compile-time). It is a dev comparator, not a run path;
  in an install it fails with a clear missing-fixture error rather than a wrong
  result.
* **`acos-cli/src/bench*.rs`** use `CARGO_MANIFEST_DIR` for the job DEX. Benchmarks
  stay a checkout tool.
* **`acos-serve` daemon**: not started during this track; the wrapper exports
  `ACOS_HOME`, and `serve` re-execs `current_exe()`, so it inherits the install.
  Untested end to end (no daemon was left running).
* Decision: mutable state uses the **existing relative paths under `ACOS_HOME`**
  (`<root>/.cache/...`), not a separate `<root>/state/`. Reason: the whole code
  base already derives state from the same root; a second constant would be a
  second rule to keep in sync, and it would have broken the "same relative
  layout, dev repo byte-identical" requirement for `/data`, sessions and the
  build cache.
* Decision: the guest Java sources are installed at
  `<root>/crates/acos-art/guest/java` (the checkout's relative path) rather than a
  tidier `<root>/share/guest`, for the same reason — one rule, no special case.
* Decision: the wrapper pins `ACOS_HOME` to the versioned directory, not to
  `current`, so a concurrent upgrade cannot swap the tree under a running
  session.
* Not done: `acos doctor` does not attempt to *launch* a guest (it stays
  read-only); a `--runtime` smoke would need the ART lock, out of scope here.
* Disk: the install (1.5 GB, 1.7 GB with its created state) was measured while
  `/` sat at 85 %; it was removed immediately after acceptance because of the
  ceiling in the operating directives. Peak observed during this track: 85 %
  (759 G of 946 G used, 139 G free) before the delete; 81 % after.

## 5. Reproduce

```bash
cd /home/Qui/.paseo/worktrees/3oxa6xao/h4
export CARGO_TARGET_DIR=.cache/h4-target
/home/Qui/Projects/acos/tools/guarded.sh --mem 12G --time 900 -- \
  cargo build --release --offline -p acos-cli -p acos-art -p acos-mcp
./tools/release/install.sh --from-target .cache/h4-target \
  --prefix $PWD/.cache/rel1a/prefix --version 0.2.0

mkdir -p /home/Qui/Projects/acos/.cache/rel1a-outside
cd /home/Qui/Projects/acos/.cache/rel1a-outside
P=<WT>/.cache/rel1a/prefix/bin
$P/acos doctor
$P/acos inspect /home/Qui/Projects/acos/fixtures/build/apps/counter.apk
/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task REL1a -- \
  /home/Qui/Projects/acos/tools/guarded.sh --mem 10G --time 600 -- \
  $P/acos verify --engine art --scenario <WT>/.cache/rel1a/scenarios/v2-art-counter.yaml --artifacts <WT>/.cache/rel1a/evidence/counter
/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task REL1a -- \
  /home/Qui/Projects/acos/tools/guarded.sh --mem 10G --time 600 -- \
  $P/acos verify --engine art --scenario <WT>/.cache/rel1a/scenarios/gaten-home.yaml --artifacts <WT>/.cache/rel1a/evidence/gaten
```
