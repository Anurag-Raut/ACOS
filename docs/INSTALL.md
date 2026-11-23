# Installing ACOS

ACOS runs from a checkout **or** from an install root. An install is a copy of
the runtime trees plus the binaries; with `ACOS_HOME` set, every runtime path
resolves under that root and no checkout is needed (or touched).

## Install

```bash
tools/release/install.sh --prefix ~/.local --version 0.4.1
# build the five release binaries and install (idempotent; ~1.5 GB)
```

```bash
cargo build --release -p acos-cli -p acos-art -p acos-mcp -p acos-netd -p acos-webview
tools/release/install.sh --prefix ~/.local --from-target target
# reuse binaries already built in <dir>/release
```

Options: `--prefix DIR` (default `$ACOS_PREFIX`, else `~/.local`), `--version V`
(default `0.4.1`), `--from-target DIR`, `--pasta PATH`, `--force`,
`--register-mcp`. It prints the install size and the next steps. No root, no
downloads. `--force` is
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
<prefix>/share/acos/current -> 0.4.1
<prefix>/share/acos/0.4.1/
  bin/{acos,acos-art,acos-mcp,acos-netd,acos-webview}
  libexec/pasta                   the pinned static transport (ADR-0030)
  share/doc/acos/pasta/           COPYING (GPL-2.0-or-later), BSD-3-Clause.txt, SOURCE.md
  VERSION, MANIFEST.txt
  .cache/art/sysroot/             ART sysroot, the framework (~860 MB)
  .cache/boot/android-34/         fonts, static overlays, BOOTCLASSPATH (~250 MB)
    system-apps/                  the system app packages the image ships
      com.android.webview.apk     the ACOS WebView provider (ADR-0031)
  crates/acos-art/guest/java/     guest sources, compiled at run time
  research/art-feasibility/a-dalvikvm/bootclasspath.txt   (legacy fallback)
```

The relative layout is deliberately the checkout's: the code has one rule
(`acos_art::repo_root()` = `$ACOS_HOME` else the checkout), so an install is a
copy, not a second code path.

Internal symlinks inside those trees are preserved (some are dangling in the
source extraction too); the installer fails closed if the copied tree contains a
link that resolves outside the install root.

### The WebView provider ships in the image

`webview: real` (ADR-0029/ADR-0031) is the only module that installs a **system
app**: the framework's `WebViewFactory` loads a provider package, and ACOS's
provider is `com.android.webview`. That APK is part of the image, so a fresh
install needs no separate build step:

* the image build, `tools/fetch/extract-bootclasspath.sh`, builds the provider with
  `tools/build/build-webview-provider.sh` and stages it at
  `.cache/boot/android-34/system-apps/com.android.webview.apk`, listing it in the
  image's `manifest.json` under `system_apps`. An image built before
  `system-apps/` existed is upgraded in place by the same script (no
  re-extraction); `--system-apps-only` forces just that step;
* the installer copies it with the rest of the image, and if the source image
  does not have it, the installer builds it into the install root (the one build
  implementation is `tools/build/build-webview-provider.sh`); its report says which
  path it used;
* the `webview` module resolves the provider as `config.provider_apk` →
  `ACOS_WEBVIEW_PROVIDER_APK` → the image's copy → the fixture build
  (`fixtures/build/apps/webview-provider.apk`, what `tools/build/build-apps.sh`
  produces for a bare checkout), and its `status` control op names the file it
  used — a wrong or missing provider is visible in the reply, never a WebView
  that paints nothing.

### The network module's transport: `libexec/pasta`

ADR-0030's `network` module (`network:real`) runs a session's traffic through one
**static `pasta` build of a pinned tag** per host, and the tag is not a
detail: it is the transport the relay's boundary, host-port opt-in and
online/offline controls were measured on. So the binary has one documented home
and one lookup rule (`crates/acos-netd-ctl/src/pasta.rs`):

| order | location |
| --- | --- |
| 1 | `$ACOS_NET_PASTA` — an explicit path (a spike, a locally built binary) |
| 2 | `<root>/libexec/pasta` — `$ACOS_HOME` when installed, else the checkout root |
| 3 | `$ACOS_PREFIX/share/acos/current/libexec/pasta` |
| 4 | `pasta` on `PATH` |

An install carries its own copy: `tools/release/install.sh` copies the binary it
finds (`--pasta PATH`, `$ACOS_NET_PASTA`, `<repo>/libexec/pasta`, or `pasta` on
`PATH`) to `<prefix>/share/acos/<version>/libexec/pasta`, after checking the
tag it reports and the sha256 recorded in
`research/spike-net/artifacts/passt-build.txt` (tag `2026_10_02.cba3570`,
sha256 `314c37e8…edf60`). A binary that is not that build is **not** installed:
the installer prints the recipe and the install keeps working without a network.

Building it yourself is four commands (no root, no downloads beyond the source):

```bash
git clone --depth 1 -b 2026_10_02.cba3570 https://passt.top/passt pasta-src
make -C pasta-src static VERSION=2026_10_02.cba3570
sha256sum pasta-src/pasta   # must be 314c37e878bd6ffe43200735800d5efd4859d81d46b0450358a103c36c0edf60
install -m 755 pasta-src/pasta "$ACOS_HOME/libexec/pasta"    # or: install.sh --pasta …
```

`pasta` is GPL-2.0-or-later (its own `LICENSES/` ship with the source); it is a
separate executable, never linked into ACOS. Whenever `tools/release/install.sh`
ships `libexec/pasta`, it also installs the verbatim upstream licence text at
`share/doc/acos/pasta/COPYING` (and `BSD-3-Clause.txt`) plus a source and
GPL-2.0 §3 written-offer notice at `share/doc/acos/pasta/SOURCE.md`, and lists
them in `MANIFEST.txt`.

Who reports it:

* `acos doctor` prints `pasta: <version> (<path> [<source>])` when it resolves,
  and `pasta: missing (searched: …)` otherwise — a `warn`, because the default
  network profile is `off` and nothing else needs it;
* `acos-netd pasta [--json]` prints the same resolution and exits 1 when it is
  missing, so a script (the scenario runners) resolves a scenario's
  `needs_env: [pasta]` with the product's own rule;
* the relay's `status` names both the version it found and the expected tag
  (`cargo`/`PASTA_TAG`), so a wrong binary is visible, never silently used;
* `tools/verify/run-matrix.sh` skips a `needs_env: [pasta]` scenario (and says so in
  `skipped_needs_env`) instead of turning it into a red row that is not the
  scenario's fault.

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
* the `network` module's `real` profile only: the pinned `pasta` (above);
  without it `doctor` says `pasta: missing`, the module refuses to attach with
  `NETWORK_UNAVAILABLE`, and the `needs_env: [pasta]` scenarios are skipped;
* fixtures only: `zipalign`, `apksigner`, `keytool`, `tools/build/apk_append.py`.

With `ACOS_HOME` set, doctor checks the install itself (root, `VERSION`, sysroot,
fonts, overlays, guest sources, `acos-art`, `install_webview`, writable state)
and stops treating a missing checkout as a failure. The MCP server follows the
same rule: its runtime paths (boot-classpath cache, engine/RN driver assets)
resolve under `ACOS_HOME` exactly like the CLI's, so an installed server launched
from another repository reads the installed tree and never a checkout.

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
tools/release/install.sh --prefix ~/.local --version 0.4.1   # adds 0.4.1, flips current
```

## Uninstall

```bash
rm -rf <prefix>/share/acos/<version> <prefix>/bin/acos <prefix>/bin/acos-mcp
rm -rf <prefix>/share/acos          # only when no other version is left
```

Nothing is installed outside the prefix, and nothing in the source tree is
touched, so that is the whole uninstall.
