# WV4 — WebView track, foundation part: wall 8 (the session socket)

Branch `wv4` (from `wv3` @ `6fcbe98a`). WV3's wall 8 was

```
E : No implementation found for void android.net.LocalSocketImpl.connectLocal(
        java.io.FileDescriptor, java.lang.String, int)
    at android.net.LocalSocketImpl.connectLocal(Native Method)
    at com.android.webview.chromium.AcosSessionSocket.open(AcosSessionSocket.java:30)
```

Two things were missing and both are fixed here, at the root cause, with no
fake transport anywhere:

1. **the natives** — `android.net.LocalSocketImpl`'s seven JNI methods were
   never registered in the real-ART guest, and
2. **the path** — the guest's `/run/acos/webview.sock` was not mapped to any
   host path, so even a registered native had nothing to connect to.

## 1. The natives: the image's own `register_android_net_LocalSocketImpl`

ACOS does not re-implement framework natives for the real-ART session: it calls
the image's exported `android::register_*` entry points from
`libandroid_runtime.so` (`RenderNat.register`, `crates/acos-art/shim/
render_shim.c`), and those register AOSP's own implementations. That is the
"same place" every other `android.os`/`android.net` native comes from, and the
WebView transport is no exception:

```
$ nm -D --defined-only .cache/art/sysroot/system/lib64/libandroid_runtime.so \
    | grep -i localsocket
0000000000181820 T _ZN7android36register_android_net_LocalSocketImplEP7_JNIEnv
```

`base/core/jni/android_net_LocalSocketImpl.cpp` registers exactly the methods
`LocalSocket`'s connect path needs — `connectLocal`, `bindLocal`,
`read_native`, `readba_native`, `write_native`, `writeba_native`,
`getPeerCredentials_native` — plus the `inboundFileDescriptors` /
`outboundFileDescriptors` field ids (both present in the image's
`framework/classes2.dex`). `create()`/`close()`/`shutdown*()`/`getOption` are
**not** natives in API 34: they are Java over `libcore` `Os.socket`/`Os.close`/
`Os.shutdown`/`Os.getsockopt*`, i.e. the real syscalls.

`connectLocal` is a real `connect(2)` (`socket_local_client_connect`,
libcutils) with namespace semantics kept intact:

| `LocalSocketAddress.Namespace` | sockaddr |
|---|---|
| ABSTRACT | `sun_path[0]=0`, name follows (abstract namespace) |
| RESERVED | `@name` → `sun_path[0]=0`, `name` follows |
| FILESYSTEM | the name as a filesystem path (what the provider uses) |

and `ret < 0 → jniThrowIOException(env, errno)`: the guest sees the kernel's
real errno as an `IOException`, never a fabricated success.

Where it is registered (one line each, with the wall in the comment):
`crates/acos-art/guest/java/dev/acos/art/LaunchMain.java` (`registerNatives`,
the app-launch route the WV3 scenario takes) and
`crates/acos-art/guest/java/dev/acos/art/SessionMain.java` (`stepRuntimeExtras`,
for the other ART guest route) — the native is missing for *every* guest, so it
is registered once for the whole class.

## 2. The path: a per-session host directory mounted at `/run/acos`

`crates/acos-art/src/webview_socket.rs` is the whole mapping and nothing else:

| guest | host |
|---|---|
| `/run/acos` (read-only mount point) | `<dir>` = `$ACOS_WEBVIEW_SOCKET_DIR` else `<run_root>/wv` |
| `/run/acos/webview.sock` | `<dir>/webview.sock` |

* `Sandbox::command` (`crates/acos-art/src/lib.rs`) adds
  `--ro-bind <dir> /run/acos` to **every** run's bwrap argv. It is a
  *directory* mount, not a file mount, so the broker may create its listener at
  any moment after the guest starts and the guest still sees it; read-only
  means the guest can `connect()` to a node it cannot unlink.
* `launch_session` (`crates/acos-art/src/launch.rs`) resolves the directory
  before the guest exists, refuses a relative override and a host path over the
  107-byte `sockaddr_un` budget (a structured `Layout` error naming
  `ACOS_WEBVIEW_SOCKET_DIR`, instead of an unattributable `ENAMETOOLONG` inside
  the WebView), and publishes the host path in `LaunchResult.notes`:
  `webview socket: <host> -> guest /run/acos/webview.sock (…)`
  (`evidence/ev1-launch-note.txt`).
* **No broker ⇒ no listener ⇒ the kernel's real `ENOENT`**, which
  `AcosSessionSocket.open` turns into the structured `ENGINE_UNAVAILABLE`
  (§`results/handoff/WV3/WALLS.md` §4 predicted exactly this failure). The
  broker (`crates/acos-webview`, branch `wvb`) is the only thing to add; the
  session-side work is done.
* The default subdirectory is `wv`, not `webview`: `sun_path` is 108 bytes and
  a run root is already 88 from this checkout's `.cache/art/data` (the existing
  logd socket is `<run>/socket/logd` at 100). `<run>/wv/webview.sock` is 104, so
  the guest-visible file name stays and the mapping fits.

Nothing in the guest changed: the provider still connects to the literal
`/run/acos/webview.sock` (`WIRE.md` §0) with `Namespace.FILESYSTEM`.

## 3. Evidence

| file | what it shows |
|---|---|
| `evidence/ev1-no-broker-logcat.txt` | arm 1 (no broker). Line 87 `create(WebViewDelegate present)`, line 88 `ACOS WebView engine unavailable: /run/acos/webview.sock: java.io.IOException: No such file or directory` — **no `UnsatisfiedLinkError`, no native frame**: the guest's real `connectLocal` ran and the kernel said ENOENT |
| `evidence/ev1-no-broker-verdict.txt` | the tightened scenario's verdict: `status: fail`, `ASSERTION_FAILED` on `assert_text "WebView created:"`, note *"negative control failed exactly as declared"* — the run can no longer pass while the WebView is not constructed |
| `evidence/ev1-launch-note.txt` | the default host path of that session: `<data>/runs/<id>/wv/webview.sock` (no `ACOS_WEBVIEW_SOCKET_DIR`) |
| `evidence/ev2-listener-wire.log` | arm 2 (test listener on the mapped host path): `listener: guest connected`, `guest->host {"client":"acos-webview/0.4.0","id":"g1","op":"hello","proto":1}`, the WIRE v1 reply, `listener: guest closed` — the guest's connect and the framed `hello` round-trip |
| `evidence/ev2-listener-logcat.txt` | same run's guest side: no transport error at all; the WebView constructor proceeds past `createWebView` into the provider's own `ViewDelegate.setOverScrollMode` (a WV2 guest-provider gap, not a transport wall) |
| `evidence/ev2-listener-verdict.txt` | that arm's verdict (assertion on "WebView created:" still fails — the provider surface stops first; see §4) |
| `evidence/minimal-listener.py` | the listener that produced ev2 (a dumb WIRE v1 reply, **not** a broker) |
| `evidence/host-tests-acos-art-lib.txt` | `cargo test -p acos-art --lib` → **159 passed, 0 failed, 1 ignored** |
| `evidence/host-tests-webview-socket.txt` | the new tests, named (mapping, listener round-trip, ENOENT/ECONNREFUSED, mount argv) |

## 4. Tests

`crates/acos-art/src/webview_socket.rs` (5 unit tests):

* `guest_path_is_the_mount_point_plus_the_socket_file` — the provider's literal
  decomposes into the mount point + the mounted file name.
* `default_host_dir_is_the_runs_own_webview_directory` — `<run>/wv`, mode 0700,
  created before the broker, inside `sun_path`.
* `env_override_wins_and_must_be_absolute` — the session-owner seam; a relative
  override and a too-long host path are refused with errors that name the
  variable and the limit.
* `listener_at_the_mapped_path_receives_the_connect_and_bytes_round_trip` — a
  real `UnixListener` at the mapped host path accepts the client's real
  `UnixStream::connect` (the same `connect(2)` the native makes) and a WIRE
  frame (`uint32_be` length, kind 1, JSON) arrives byte-identical and is
  answered.
* `without_a_broker_the_connect_fails_with_the_kernels_enoent` — no listener ⇒
  `ENOENT`; a stale socket node ⇒ `ECONNREFUSED`. Both are surfaced, neither is
  papered over.

`crates/acos-art/src/lib.rs` (`tests::run_root_maps_the_webview_session_socket`)
— the bwrap argv of a run contains `--ro-bind <run>/wv /run/acos`, never a
writable `--bind` of it, and staging (a host build step with no guest) does not
mount it.

Fails-before / passes-after: the parser arm's *before* is WV3's wall-8 log
(`WALLS.md` §4, `UnsatisfiedLinkError` at `connectLocal`); the mount arm's
*before* is the same run — with no mount, a listener on the host path can never
be reached (`/run` inside bwrap is a tmpfs the host directory is not part of),
which is exactly what arm 2 now contradicts.

## 5. Tightened scenario (no more false pass)

`results/handoff/WV3/webview-probe-provider.yaml` (corrected in place, copied
to `results/handoff/WV4/`) used to `wait_for` the `status` view only — a run
where the WebView constructor threw still reported **pass**. It now:

```yaml
expectation: fail
expect_error: {code: ASSERTION_FAILED}
…
  - assert_text:
      selector: {id: status}
      text: {contains: "WebView created:"}
```

so the run is a pinned negative: `fail` + `ASSERTION_FAILED` **is** the current
truth and any `pass` is a harness error ("fail expectation was violated by a
pass"). When the broker lands, flip `expectation` to `pass` and drop
`expect_error`; nothing else changes. `webview-probe-provider-with-listener.yaml`
is the same scenario with the flipped expectation, used only for arm 2.

## 6. What remains (not this track)

* **The host broker** (`crates/acos-webview`, WVB). It binds
  `<dir>/webview.sock` — `<dir>` being `ACOS_WEBVIEW_SOCKET_DIR` when a session
  owner pins it (the daemon should, so the path outlives one run), else the
  run's `wv/`. The socket is created after the guest starts (the mount is a
  directory, so that is expected), and its owner is the host user, which the
  guest's `--unshare-user --uid` maps to its own uid, so a 0600 node is
  connectable; 0666 is only needed if the guest uid ever stops being the mapped
  one.
* **The provider surface** the constructor reaches next:
  `ViewDelegate.setOverScrollMode(I)` (and the rest of the generated
  `WebViewChromiumViewDelegate`) is still an unsupported-method throw from WV2's
  provider. That is wall 9: guest provider code, not foundation.

## 7. Commands used

```sh
# host tests
CARGO_TARGET_DIR=$WT/.cache/wv4-target CARGO_PROFILE_DEV_DEBUG=0 \
CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
tools/guarded.sh --mem 12G --time 900 -- cargo test --offline -p acos-art --lib

# fixtures
ACOS_APP_ONLY="webview-probe webview-provider" tools/build-apps.sh

# ART arm 1 (no broker; default mapping)
ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts \
ACOS_RUNTIME_DIR=$PWD/.cache/wv4-run ACOS_SESSION_SOCKET=$PWD/.cache/wv4-run/session.sock \
ACOS_ART_BIN=$PWD/.cache/wv4-target/release/acos-art \
tools/art-runtime/run-exclusive.sh --task WV4 -- tools/guarded.sh --mem 8G --time 600 -- \
  ./.cache/wv4-target/release/acos verify .cache/wv4-run/scenario.yaml --engine art \
  --artifacts .cache/wv4-run/ev1

# ART arm 2 (minimal test listener on the mapped host path)
mkdir -p .cache/wv4-run/wv
python3 results/handoff/WV4/evidence/minimal-listener.py \
  $PWD/.cache/wv4-run/wv/webview.sock $PWD/.cache/wv4-run/listener.log &
ACOS_WEBVIEW_SOCKET_DIR=$PWD/.cache/wv4-run/wv … \
  ./.cache/wv4-target/release/acos verify .cache/wv4-run/scenario-listener.yaml \
  --engine art --artifacts .cache/wv4-run/ev2
```
