# RELFIX4 — v0.4.0 release blocker fix & Step 8 re-run

Branch `relfix4`, branched from `origin/relgate4` (`46d6b0898` = `origin/modules` `afad66858` + `relnotes2h` + RELGATE4 evidence).

**Verdict: SHIP v0.4.0 — Step 8 PASS (RELFIX4); Steps 0–7 stand from RELGATE4 (`46d6b0898`) since no runtime, engine, or service-module product code changed.**

## Changes

1. **`pasta` licence + corresponding source notice (release blocker):**
   - Vendored verbatim upstream passt (`tag 2026_10_02.cba3570`, `https://passt.top/passt`) licence files under `tools/release/licenses/pasta/GPL-2.0-or-later.txt` (sha256 `aaf135472f81c5b4a0dca9367e5bb5e9750032b5bebe5442b36e4c0a47430df3`, identical to `/usr/share/licenses/spdx/GPL-2.0-or-later.txt`) and `tools/release/licenses/pasta/BSD-3-Clause.txt` (sha256 `32dfe03d9985f5feb581da26a2fe6c84530cccb6b53ede0d8dab075cd048ac5b`).
   - Updated `tools/release/install.sh` (`install_pasta`) to install `share/doc/acos/pasta/COPYING`, `share/doc/acos/pasta/BSD-3-Clause.txt`, and `share/doc/acos/pasta/SOURCE.md` (upstream URL, tag `2026_10_02.cba3570`, commit `cba357068dc586e7540971eb40bb56c5e62c4de1`, binary size and sha256, build command, and GPL-2.0 §3 written source offer/pointer) whenever `libexec/pasta` is shipped, and to list them in `MANIFEST.txt`. When no pinned `pasta` binary is shipped, neither `libexec/pasta` nor `share/doc/acos/pasta/` is installed and the existing build recipe is printed.
2. **Symlinked `pasta` size logging:**
   - Updated `tools/release/install.sh` to use `stat -L -c '%s'` so symlinked `<repo>/libexec/pasta` logs `1548232 bytes` instead of the `49 bytes` symlink length.
3. **`acos-webview` binary + installed `acos doctor` check:**
   - Verified `acos-art` (`crates/acos-art/src/webview_socket.rs` → `acos_webview::BrokerSource::from_env`) resolves `acos-webview` as a sibling of its own executable, so `webview: real` requires `bin/acos-webview` in the installed prefix.
   - Added `acos-webview` to `BINS` and the `cargo build` crate list in `tools/release/install.sh`.
   - Added `install_webview` check to `check_install` in `crates/acos-cli/src/doctor.rs` (verifying both `bin/acos-webview` and `.cache/boot/android-34/system-apps/com.android.webview.apk`) plus unit test `install_webview_check_requires_broker_and_provider`.
4. **Version bump to `0.4.0`:**
   - Bumped `[workspace.package]` in `Cargo.toml` (`0.3.0` → `0.4.0`), updated `Cargo.lock` via `cargo update -w --offline` (all 29 workspace crates), and updated `docs/INSTALL.md`.

## Verification

| Check | Result | Evidence |
|---|---|---|
| `tools/build-apps.sh` | **PASS** (`BUILD_APPS_RC=0`) | `00-build-apps.log` |
| `cargo build --release --workspace --offline` | **PASS** (`BUILD_RC=0`, 59.82 s) | `01-build-release.log` |
| `cargo test -p acos-cli --release --offline` (`env -u TMPDIR`, `ACOS_S8_LIVE_HOSTILE=1`, via `run-exclusive.sh`) | **PASS** (`CLI_TEST_RC=0`, **315 passed / 0 failed / 15 ignored / 0 skipped**) | `02-test-acos-cli.log` |
| Step 8 install (`tools/release/install.sh --prefix .cache/rf4-prefix --from-target .cache/rf4-target`) | **PASS** (`INSTALL_RC=0`, `1.5G`, `3189` files in `MANIFEST.txt`, `pasta` logged `1548232 bytes`, `pasta docs: share/doc/acos/pasta/COPYING, share/doc/acos/pasta/SOURCE.md`) | `03-install.log` |
| Step 8 `acos --version` | **PASS** (`acos 0.4.0`) | `04-version-doctor.log` |
| Step 8 `acos doctor --json` | **PASS** (`ok: true`, **31/31 checks `ok`**, `layout: installed`, `acos_version: 0.4.0`, `install_webview: ok` [broker + provider], `pasta: ok 2026_10_02.cba3570 [acos_home]`, `gles: ok angle-swiftshader`) | `04-version-doctor.log` |
| Step 8 `pasta` binary + `COPYING` + `BSD-3-Clause.txt` + `SOURCE.md` + `MANIFEST.txt` | **PASS** (`libexec/pasta` 1,548,232 bytes sha256 `314c37e8…edf60`; `COPYING` 17,337 bytes sha256 `aaf13547…430df3`; `BSD-3-Clause.txt` 1,547 bytes; `SOURCE.md` 2,470 bytes; all listed in `MANIFEST.txt`) | `04-version-doctor.log` |
| Step 8 `acos verify scenarios/v2-art-counter.yaml --engine art --json` (via `run-exclusive.sh`) | **PASS** (`VERIFY_RC=0`, `status: pass`, `fidelity: art-reference`) | `05-prefix-verify-counter.jsonl`, `05-prefix-verify-counter.verdict.json` |
| Step 8 MCP `initialize` + `tools/list` | **PASS** (`{"name": "acos-mcp", "version": "0.4.0"}`, protocol `2025-03-26`, **22 tools**, capability probe 8 commands) | `06-mcp-roundtrip.jsonl`, `06-mcp-roundtrip.log` |
| Negative check: install without pinned `pasta` (`--pasta /nonexistent/pasta`) | **PASS** (`NOPASTA_INSTALL_RC=0`, `libexec/pasta` absent, `share/doc/acos/pasta` absent, 0 `pasta` lines in `MANIFEST.txt`, `3185` files, build recipe printed) | `07-install-nopasta.log` |

## Environment & Safety

- Private `ACOS_RUNTIME_DIR` + `ACOS_SESSION_SOCKET` under `$WT/.cache/rt-rf4*`; `ACOS_HOME`, `ACOS_BOOT_DIR`, `XDG_RUNTIME_DIR` unset for installed smokes; `~/.local`, `~/Projects/gaten`, and physical phone untouched; no tag created.
- Peak RAM observed: 16 / 60 GiB (~27% used); disk stayed at 83% (`157–160 GiB` free).
- Temporary directories (`.cache/rf4-target`, `.cache/rf4-prefix`, `.cache/rt-rf4*`, `fixtures/build`) deleted before commit.
