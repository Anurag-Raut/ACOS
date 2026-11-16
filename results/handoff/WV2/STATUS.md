# WV2 — WebView track, session 2 (guest side)

Status: **PARTIAL, with the WV1 blocker closed.** Wall 4 (the reply that
desynchronised the framework's `PackageInfo` reader) is fixed at the root cause
and proven in a real ART session; the provider-load path now advances to the
**next** missing foundation API (wall 5, `IActivityManager.addPackageDependency`).
The guest provider's method surface is implemented against the broker's wire
protocol (WVB/WIRE.md, conformed to) with a stand-in transport, but cannot be
executed in a session until walls 5–7 open.

## 1. What changed

1. **`ACOS_PARCEL_DUMP=<path>`** beside `ACOS_PMS_TRACE`
   (`crates/acos-services/src/parcel_dump.rs`): one `.bin` per reply plus an
   index. Wired at the `pms.getPackageInfo` codec view, at the `webviewupdate`
   peer's reply, and at the transport's final (post-materialiser) reply.
2. **Wall 4 root cause**: a method that returns a *parcelable* writes the return
   value's presence word **and** the parcelable's own nested
   `writeTypedObject(packageInfo)` presence. `WebViewUpdateService` wrote one,
   so the framework read `packageName`'s length as the `packageInfo` presence and
   took `splitNames`'s count out of the string bytes (the deterministic
   `7 676 737 936`-byte allocation at `PackageInfo.<init>(PackageInfo.java:570)`,
   the *first* `createString8Array` per the image's own line table). Fixed in one
   place; both regression tests are real-framework round trips, and one of them
   proves the `PackageInfo`/`ApplicationInfo` codecs were innocent.
3. **The AOSP tag lied about three fields** (`optionalSharedLibraryInfos`,
   `allowCrossUidActivitySwitchFromBelow`, `mArchiveTimeMillis`/
   `mApexPackageName`): the image's `classes.dex` does not write them. Checked
   with `dexdump`, so no wrong "fix" was committed (WALLS.md §2).
4. **Provider Java**: `WebViewChromiumCore` + `AcosWebViewTransport` +
   `AcosSessionSocket`, with the generated classes forwarding to them
   (`tools/gen-webview-provider.py` regenerated; `--check` clean). APK builds
   (33 171 bytes) and is installed by the existing scenario.
5. **`results/handoff/WV2/WIRE.md`**: the guest side of the normative
   `results/handoff/WVB/WIRE.md` (branch `wvb`), conformed to, no additions.

## 2. Evidence

| item | file |
|---|---|
| pre-fix reply bytes (the 4-byte shift) | `evidence-1/reconstructed-before-fix-head.hex`, `evidence-1/webviewupdate.2.reconstructed-before-fix.bin`, pre-fix OOM in `evidence-1/logcat-before-parcel-fix.txt` |
| post-fix reply head `[0][1][1]` | `evidence-1/after-fix-head.hex` |
| post-fix session: OOM gone, wall 5 named | `evidence-1/logcat-after-parcel-fix.txt` (78–83), `evidence-1/wall5-addPackageDependency.txt` |
| reply dump index / PMS trace | `evidence-1/index.txt`, `evidence-1/pms.trace.txt` |
| scenario + verdict | `webview-probe-provider.yaml`, `evidence-1/verdict.after-parcel-fix.json` |
| wall map | `WALLS.md` |
| verdicts | `evidence-1/verdict.before-parcel-fix.json`, `evidence-1/verdict.after-parcel-fix.json` |

Commands (private runtime dir + socket, shared lock):

```sh
ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts \
ACOS_RUNTIME_DIR=$PWD/.cache/wv2-run ACOS_SESSION_SOCKET=$PWD/.cache/wv2-run/session.sock \
ACOS_ART_BIN=$PWD/.cache/wv2-target/release/acos-art \
ACOS_PARCEL_DUMP=$PWD/.cache/wv2-run/parcel ACOS_PMS_TRACE=$PWD/.cache/wv2-run/pms.trace \
tools/art-runtime/run-exclusive.sh --task WV2 -- tools/guarded.sh --mem 8G --time 600 -- \
  ./.cache/wv2-target/release/acos verify results/handoff/WV2/webview-probe-provider.yaml \
  --engine art --artifacts .cache/wv2-run/ev2
```

Tests (VM + real framework, no ART lock):

```sh
cargo test -p acos-runtime --test parcelable_java webview_provider
cargo test -p acos-services --lib webview
```

## 3. Not done (smallest next action each)

| item | next action |
|---|---|
| wall 5 `addPackageDependency` | AMS method (record the dependency, no exception reply) — AMS surface, not WebView |
| walls 6–7 (`createApplicationContext` for a non-launched package, provider classloader, `create()`/`new WebView`) | foundation work; report as an architectural wall if it needs more than the ADR-0029..0031 shape |
| provider methods executing against a real broker | after wall 7: point `AcosSessionSocket.open` at the broker client (`AcosWebViewTransport.Transport`), then the gate scenario |
| gates (a)–(e) in-session | same |
