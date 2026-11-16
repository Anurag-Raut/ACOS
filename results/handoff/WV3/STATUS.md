# WV3 — WebView track, guest side, session 3

Status: **PARTIAL, and the provider now genuinely instantiates.** Walls 5, 6 and
7 of `WebViewFactory.getProvider()` are closed; the framework loads the
provider package, builds its classloader, and runs
`WebViewChromiumFactoryProviderForT.create(WebViewDelegate)` in a real ART
session. The next wall is the provider's transport: the guest has no
`android.net.LocalSocketImpl.connectLocal`, so the WebView constructor cannot
finish. That wall is a VM/native + session-socket-creation item and is
reported, not worked around (§4 of `WALLS.md`).

## 1. What changed

| commit part | file | what |
|---|---|---|
| wall 5 | `crates/acos-services/src/ams.rs` | `IActivityManager.addPackageDependency` dispatch arm + `AmsState::add_package_dependency` + `AmsState.package_dependencies` (the `ProcessRecord.pkgDeps` record), void reply, empty name refused |
| wall 5 test | same file | `add_package_dependency_is_recorded_and_replies_void` — pins the image AIDL shape (code 96, one `Str16`, empty reply) and the behaviour; measured failing before, passing after |
| wall 6 | `fixtures/apps/webview-provider/AndroidManifest.xml` | declares `com.android.webview.WebViewLibrary` (required by `verifyPackageInfo`) with the AOSP-degraded load documented |

No framework hack, no new wire field, no `acos-aidl` table change (it was
already right: the decoded shape is code 96, `[Str16]`, empty reply).

## 2. Acceptance criteria

| criterion | verdict | evidence |
|---|---|---|
| wall 5 implemented in `ams.rs` + table checked against the image dex | **PASS** | `evidence/wall5-aidl-shape.txt`, `evidence/wall5-test-before-after.txt` |
| wall 5 regression test fails before / passes after | **PASS** | `evidence/wall5-test-before-after.txt` (both runs) |
| `WebViewChromiumFactoryProviderForT.create(WebViewDelegate)` runs | **PASS** | `evidence/ev2-provider-load.txt` line 87 `I ACOSWvProvider: create(WebViewDelegate present)` |
| the fixture constructs a WebView | **FAIL** | `evidence/ev2-provider-load.txt` lines 88–89: `UnsatisfiedLinkError … LocalSocketImpl.connectLocal` |
| provider reaches the WIRE socket path | **PASS (attempt recorded)** | same lines: `AcosSessionSocket.open` → `LocalSocket.connect(/run/acos/webview.sock)` → wall 8 |
| end-to-end page load + one frame | **FAIL (not possible in this tree)** | no broker in the repo: `git ls-tree -r wvb` = `results/handoff/WVB/WIRE.md` only, `crates/acos-webview` does not exist |
| every wall recorded | **PASS** | `WALLS.md` |
| tests | **PASS** | `cargo test -p acos-services --lib add_package_dependency` → 1 passed |

## 3. Commands

Tests (VM-free, VM-free crates only):

```sh
CARGO_TARGET_DIR=$WT/.cache/wv3-target CARGO_PROFILE_DEV_DEBUG=0 \
CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
tools/guarded.sh --mem 12G --time 900 -- \
  cargo test --offline -p acos-services --lib add_package_dependency
```

Session (private runtime dir + socket, shared lock, one acquisition):

```sh
ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts \
ACOS_RUNTIME_DIR=$PWD/.cache/wv3-run ACOS_SESSION_SOCKET=$PWD/.cache/wv3-run/session.sock \
ACOS_ART_BIN=$PWD/.cache/wv3-target/release/acos-art \
ACOS_PARCEL_DUMP=$PWD/.cache/wv3-run/parcel \
tools/art-runtime/run-exclusive.sh --task WV3 -- tools/guarded.sh --mem 8G --time 600 -- \
  ./.cache/wv3-target/release/acos verify .cache/wv3-run/scenario.yaml \
  --engine art --artifacts .cache/wv3-run/ev2
```

Fixtures: `ACOS_APP_ONLY="webview-provider webview-probe" tools/build-apps.sh`.

## 4. Not done

| item | smallest next action |
|---|---|
| `LocalSocketImpl.connectLocal` | VM native: create/connect `AF_UNIX` in the host process, map the guest path through the VFS, register the fd in `Vfs` (`Source::File` slots already support host sockets) |
| the session socket itself | host side: create the WebView session socket during launch and expose it at the guest path WIRE.md names (`/run/acos/webview.sock`); needs the broker (WVB) to be in the tree — it is not |
| broker end-to-end | the `wvb` branch must land `crates/acos-webview` (its `wire.rs`) before any page/frame can be measured |
| gates (a)–(e) | after the two items above |
