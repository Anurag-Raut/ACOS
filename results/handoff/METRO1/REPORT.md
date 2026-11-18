# METRO1 — an RN debug app fetches its JS from host Metro (v0.5.0 step 1)

**Branch** `metro1` (from `modules` `aaf518bf8`). **Status: PARTIAL** — the
network leg is proven on real ART (a debug RN 0.81 app in an ACOS session fetched
its bundle from a Metro dev server on the host and rendered the bundle's text);
the reload leg is *delivered but not shown*: the new JS context runs, but the view
tree is not replaced, and the cause is located (METRO2).

Plan for the whole track: `docs/METRO_PLAN.md` (METRO1..8).

## 1. What runs

```
pasta pasta 2026_10_02.cba3570                            # pinned binary, sha256 314c37e8… matched
metro up pid=95658 port=8081 project=…/.cache/metro1/app2 # RN 0.81 CLI dev server (Metro 0.83.8)
host_bundle http=200 bytes=4937649                        # /status 200, dev bundle 4.9 MB
netd_up rc=0 pasta="pasta_version": "2026_10_02.cba3570"  # ADR-0030 relay (one pasta per host)
hostport_add rc=0 … host_ports … 8081                     # relay-level opt-in, BEFORE the guest
launch rc=0 stage=pass                                    # acos launch --engine art, network:real
text_found 'Hello ACOS' after 0s                          # the bundle's text, on the first observe
reload http=200 body=OK                                   # POST /reload == the CLI's `r` key
host_bundle_after_edit served=NEW_TEXT                    # Metro rebuilt; the server side is right
TIMEOUT 'Hello Metro' after 90s                           # the guest did not re-render it
RESULT metro1=FAIL stage=reload
```

Full transcript: `run/metro1.log`. Driver: `tools/metro/metro1-dev-loop.sh`.

## 2. Acceptance, criterion by criterion

| # | criterion | verdict | evidence |
|---|---|---|---|
| a | `docs/METRO_PLAN.md` breaks METRO1..8 down (bundle fetch, reload, fast refresh/HMR, redbox as verdict data, dev menu / adb-reverse equivalent, source maps, Hermes debugger, multi-session Metro) | **PASS** | `docs/METRO_PLAN.md` |
| b | a minimal RN 0.81 **debug** APK exists, built from the `.cache/rn-0.81.4` template, and the build is recorded | **PASS** | `tools/build-rn-hello.sh debug` → `fixtures/build/apps/rn-debug.apk` (8 935 640 B; no `assets/index.android.bundle`; `getUseDeveloperSupport() == true`), §4 |
| c | network: the guest reaches host Metro (10.0.2.2 / localhost:8081 — whichever RN uses — through the module's host-port opt-in) | **PASS** | `run/netd-hostport-add.json`, `run/launch.json`, `run/observe-before.json` (`Hello ACOS`), `run/netd-up.log` |
| d1 | launch, assert text from the bundle | **PASS** | `run/observe-before.json` (ReactTextView `Hello ACOS`, 8 nodes, `stage_reached: first_frame`) |
| d2 | edit a JS file in a scratch copy, reload, assert the **new** text | **FAIL** | `run/metro1.log` (`TIMEOUT 'Hello Metro'`), `run/observe-after.json` (16 nodes, both halves `Hello ACOS`), `run/logs-reload.json` (`add a root view with an explicit id (11) already set`, `CatalystInstanceImpl.destroy() end`, `Tried to enqueue runnable on already finished thread: 'js…' dropping Runnable.`, a second `The app is running using the Legacy Architecture` = a new JS context) |

So the dev loop is proven up to and including "the JS changed and the new bundle
was fetched and started", and stops at "the screen shows the new text".

### 2b. The opt-in is load-bearing (the negative arm)

The same driver with `METRO1_SKIP_HOSTPORT=1` (`run/../negative/`, transcript
`negative/metro1.log`) launches the same APK with the same relay and no host-port
opt-in, and the guest is refused — so the pass above is the module's doing, not a
stray route:

```
RESULT negative=EXPECTED_FAIL stage=bundle_fetch (no host-port opt-in)
W ReactNative  The packager does not seem to be running as we got an IOException
               requesting its status: Failed to connect to localhost/127.0.0.1:8081
W ReconnectingWebSocket  Couldn't connect to "ws://localhost:8081/message?…will silently retry
E ReactNative  java.lang.RuntimeException: Unable to load script.
               Make sure you're running Metro or that your bundle 'index.android.bundle'
               is packaged correctly for release. … (bundler set to "localhost:8081")
```

(`negative/log-excerpt.txt`.) That is also the exact shape METRO4's
`bundle_unreachable` verdict has to name, and it shows RN's own
`ReconnectingWebSocket` is already retrying — the reason the ordering fix belongs
in the attach, not in a retry loop here.

## 3. Why the reload fails (precise, with the frame on record)

In order, from `run/logs-reload.json`:

```
E NativeViewHierarchyManager  Trying to add a root view with an explicit id (11) already set.
W ReactNative                 CatalystInstanceImpl.destroy() end
W ReactNative                 Tried to enqueue runnable on already finished thread: 'js…' dropping Runnable.
W ReactNativeJS               The app is running using the Legacy Architecture. …     ← new context, new bundle
```

and the trees (`run/observe-before.json` → `run/observe-after.json`): 8 → 16
nodes, identical bounds and identical **pre-edit** text in both halves.

Meanwhile the same bundle URL fetched by the host curl *after* the edit holds
`Hello Metro (reloaded)` and no `Hello ACOS` (`run/bundle-probes.json`:
`has_hello_acos: 0, has_hello_metro: 1`; Metro logged the rebuild). So:

* Metro, the forwarder and the network module are all doing their job;
* the reload command reached the app (`packager_connects=1, packager_closes=0` in
  that run, and a new JS context starts);
* the new context's mount work is dropped (`dropping Runnable`) and the old root
  view stays registered with the same react tag — the visible identity is stale.

That is a runtime/`ReactRootView` fidelity bug (METRO2's scope), not a
dev-server or network bug.

## 4. Build records

**Debug APK** (`tools/build-rn-hello.sh debug`, ~40 s, no Gradle): the same
`aapt2 + javac + d8 + zipalign + apksigner` toolchain as the release fixture, with
a generated `BuildVariant` (the template's `BuildConfig` stand-in):

* `BuildVariant.DEBUG = true` → `getUseDeveloperSupport() == true`;
* no `assets/index.android.bundle` in the APK (the script fails if one sneaks in);
* `MainApplication.onCreate` writes RN's own documented `debug_http_host`
  preference = `localhost:8081` (`ACOS_RN_METRO_HOST` overrides at build time).

Why `localhost` and not the stock-emulator alias: ACOS's `ro.build.fingerprint`
is `google/sdk_gphone64_x86_64/…`, so `AndroidInfoHelpers` answers `10.0.2.2`
(`isRunningOnStockEmulator()`), and no ACOS session routes to `10.0.2.2` (session
network `10.163.N.0/30`, gateway `10.163.N.1`). `localhost` is what
`acos-netd`'s per-session forwarder provides — the `adb reverse` equivalent — and
it is the address RN itself recommends for a physical device.

**Network** (one 13-line change, `crates/acos-netd/src/supervisor.rs`): a debug
RN app fetches its bundle during `Application.onCreate`, so a post-launch
`hostport add` is always too late. `apply_relay_host_ports` applies the relay's
own opt-ins (`acos-netd hostport add 8081`, no `--session`) to every session as
it attaches (spawn *and* the harness `attach`), inside the existing gate: the
per-session `ACOS-SESS` `REJECT` rules are installed first, the named port gets
its `ACCEPT` above them and its forwarder. A port that cannot be opened detaches
the session (fail closed). NET3's per-session form is unchanged, and a session
that never opted in is still refused.

**pasta**: not present on this host at all (`command -v pasta` empty, no build
cache). Rebuilt from the pinned tag `2026_10_02.cba3570`; the binary's sha256
matches `research/spike-net/artifacts/passt-build.txt` exactly
(`314c37e878bd6ffe43200735800d5efd4859d81d46b0450358a103c36c0edf60`), so this is
the same transport NET3..NET6 measured, not a new one.

## 5. Reproduce

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
tools/build-rn-hello.sh debug
git clone --depth 1 -b 2026_10_02.cba3570 https://passt.top/passt .cache/metro1/build/passt-src
make -C .cache/metro1/build/passt-src static -j8 VERSION=2026_10_02.cba3570
CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$PWD/.cache/metro1-target \
  cargo build --release -p acos-cli -p acos-netd -p acos-art
cp -al .cache/rn-0.81.4/jsproject .cache/metro1/app2      # Metro project (scratch JS copy)
tools/art-runtime/run-exclusive.sh --task METRO1 -- tools/guarded.sh --mem 10G --time 700 -- \
  bash tools/metro/metro1-dev-loop.sh "$PWD/results/handoff/METRO1/run"
```

`run/` is a full working directory: Metro log, netd status, launch envelope, both
observations, the reload attempt and the guest log. The driver starts and stops
everything it uses (Metro, `acos-netd`, the session) and refuses to drive a Metro
it did not start.

## 6. Not done / next actions (in order)

1. **METRO2** — make the reload replace the tree: start from
   `NativeViewHierarchyManager.addRootView` (the tag-11 clash: who removes the old
   root view?) and the `'js'`-thread runnable that is dropped after
   `CatalystInstanceImpl.destroy()`. Acceptance: one process, `assert_text` sees
   the new string and no stale copy; the tree must not double.
2. A `scenarios/*.yaml` arm for the bundle-fetch half, so CI can gate it without
   a driver (`needs_env: [pasta, metro]`), with the Metro start-up left to the
   scenario's host setup.
3. `debug_http_host` as an ACOS-side control (METRO5): today the debug variant
   bakes `localhost:8081`; a portable story is a launch-time preference an agent
   sets (`--pref debug_http_host=…`) or an `acos_net hostport` naming the address
   the app must use.
