# Installing ACOS

ACOS runs from a checkout **or** from an install root. An install is a copy of
the runtime trees plus the binaries; with `ACOS_HOME` set, every runtime path
resolves under that root and no checkout is needed (or touched).

## Install

```bash
tools/release/install.sh --prefix ~/.local --version 0.3.0
# build the three release binaries and install (idempotent; ~1.5 GB)
```

```bash
cargo build --release -p acos-cli -p acos-art -p acos-mcp
tools/release/install.sh --prefix ~/.local --from-target target
# reuse binaries already built in <dir>/release
```

Options: `--prefix DIR` (default `$ACOS_PREFIX`, else `~/.local`), `--version V`
(default `0.3.0`), `--from-target DIR`, `--force`, `--register-mcp`. It prints
the install size and the next steps. No root, no downloads. `--force` is
required only when the version directory already exists and was not left by this
installer for that version.

`--register-mcp` (default off) registers the **installed wrappers** with Claude
Code at user scope by calling `tools/release/register-mcp.sh --prefix <prefix>`;
run that script directly to print the command first (`--dry-run`) or to
re-register after moving the prefix. Other MCP clients get the equivalent
`mcpServers` JSON from the same script. See `docs/MCP.md` §1, including the
`ACOS_ALLOWED_ROOTS` path allow-list an agent in another repository needs.

## What lands where

```
<prefix>/bin/acos                 wrapper: exports ACOS_HOME, execs the binary
<prefix>/bin/acos-mcp             wrapper (same)
<prefix>/share/acos/current -> 0.3.0
<prefix>/share/acos/0.3.0/
  bin/{acos,acos-art,acos-mcp}
  VERSION, MANIFEST.txt
  .cache/art/sysroot/             ART sysroot, the framework (~860 MB)
  .cache/boot/android-34/         fonts, static overlays, BOOTCLASSPATH (~250 MB)
  crates/acos-art/guest/java/     guest sources, compiled at run time
  research/art-feasibility/a-dalvikvm/bootclasspath.txt   (legacy fallback)
```

The relative layout is deliberately the checkout's: the code has one rule
(`acos_art::repo_root()` = `$ACOS_HOME` else the checkout), so an install is a
copy, not a second code path.

Internal symlinks inside those trees are preserved (some are dangling in the
source extraction too); the installer fails closed if the copied tree contains a
link that resolves outside the install root.

### Mutable state

Created on demand, always under the same root — never in the checkout, never in
the current directory:

| path | what |
| --- | --- |
| `<ACOS_HOME>/.cache/art/data` | the guest `/data` skeleton, app data, per-run roots |
| `<ACOS_HOME>/.cache/art/build-cache` | content-addressed build cache (guest DEX, shims) |
| `<ACOS_HOME>/.cache/art-sock` | short socket dirs when `/run/user/<uid>` is unavailable |
| `<ACOS_HOME>/.cache/sessions` | native-engine session roots |
| `<ACOS_HOME>/.cache/acos/art/boot-images` | the H13b cached shared boot image |

`acos serve` and every `acos verify` keep their artifacts where you point
`--artifacts`.

## Run

```bash
acos doctor                     # preflight: is this host able to run ACOS?
acos inspect app.apk
acos verify --engine art --scenario my-scenario.yaml --artifacts out/my-run
```

`acos doctor` reports which tools are needed at **run time** and which are only
for building fixtures:

* runtime: `bwrap`, `systemd-run` (memory cgroup), the Android SDK's `aapt2` and
  `d8`, `android.jar` (`platforms/android-34`), `java`/`javac`, and the NDK's
  `x86_64-linux-android34-clang` (the JNI shims are compiled on a cold launch);
* fixtures only: `zipalign`, `apksigner`, `keytool`, `tools/apk_append.py`.

With `ACOS_HOME` set, doctor checks the install itself (root, `VERSION`, sysroot,
fonts, overlays, guest sources, `acos-art`, writable state) and stops treating a
missing checkout as a failure. The MCP server follows the same rule: its
runtime paths (boot-classpath cache, engine/RN driver assets) resolve under
`ACOS_HOME` exactly like the CLI's, so an installed server launched from another
repository reads the installed tree and never a checkout.

## Environment

| variable | meaning |
| --- | --- |
| `ACOS_HOME` | install root; unset = the checkout (the pre-REL1a behaviour) |
| `ACOS_ROOT` | force the checkout root when `ACOS_HOME` is unset |
| `ACOS_ART_SYSROOT`, `ACOS_ART_DATA` | override the sysroot / writable `/data` |
| `ACOS_ART_BUILD_CACHE` | build-cache dir, or `off` to bypass it |
| `ACOS_ART_BIN` | explicit `acos-art` binary |
| `ACOS_BOOT_IMAGE_CACHE`, `ACOS_CACHE_DIR` | override the boot-image cache root |
| `ACOS_BOOT_DIR` | explicit boot cache (`/system` files for the native engine) |
| `ACOS_SESSION_ROOT` | explicit native-engine session root |
| `ACOS_FONT_DIR` | explicit font dir (the checkout's dev-path seam) |
| `ANDROID_HOME` / `ANDROID_SDK_ROOT`, `ANDROID_NDK_HOME` | SDK / NDK for the run-time compiles |

An explicit `ACOS_HOME` always wins over the wrapper's default, so one install
can drive another tree.

## Upgrade

`current` is a symlink; the wrapper pins `ACOS_HOME` to the **versioned**
directory, so installing a new version cannot swap the tree under a running
session.

```bash
tools/release/install.sh --prefix ~/.local --version 0.3.0   # adds 0.3.0, flips current
```

## Uninstall

```bash
rm -rf <prefix>/share/acos/<version> <prefix>/bin/acos <prefix>/bin/acos-mcp
rm -rf <prefix>/share/acos          # only when no other version is left
```

Nothing is installed outside the prefix, and nothing in the source tree is
touched, so that is the whole uninstall.
