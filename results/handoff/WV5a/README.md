# WV5a — wall 9: the guest provider surface (WebView constructor + first layout/draw)

Branch `wv5a` (from `wv4` @ `73abec44`).

WV4 closed the transport (the session socket + the `LocalSocketImpl` natives) and
stopped here:

```
88 I ACOSWvProvider: create(WebViewDelegate present)
88 I WVProbe: WebView FAILED: java.lang.UnsupportedOperationException:
      ACOS WebView provider: unsupported method ViewDelegate.setOverScrollMode(I)V
   at com.android.webview.chromium.WebViewChromiumViewDelegate.setOverScrollMode(...)
   at android.webkit.WebView.setOverScrollMode(WebView.java:2668)
   at android.view.View.<init>(View.java:5497)
```

Wall 9 is guest-provider code: the framework's `new WebView(context)` reaches the
provider from **inside `View.<init>`** (`setOverScrollMode`), then through
`init`, the first measure, the first layout and the first draw. This branch
implements that walk, splitting it by one rule:

| surface | who owns it | what it does |
|---|---|---|
| `setOverScrollMode`, `setLayerType`, `setLayoutParams`, `setBackgroundColor`, `setScrollBarStyle`, `onScrollChanged`, `preDraw*`, temporary-detach | **guest** (View state) | stored in the core; nothing engine-side changes (Chromium keeps the same values in `AwContents`) |
| `onMeasure`, `setFrame`, `onSizeChanged`, `onConfigurationChanged`, `onMovedToDisplay` | **engine geometry** | `view.create` at the first real measurement, `view.setViewport` on every layout size change (device px + density) |
| `loadUrl` / `loadData` | **engine navigation** | `view.loadUrl` / `view.loadData` |
| `onWindowFocusChanged`, `onFocusChanged`, `requestFocus` | **engine focus** | `input.setFocus {focused}` |
| `onDraw` | **engine frames** | paints the last frame; asks for the next one with one coalesced `render` per invalidation |
| `getSettings` | **guest**, engine-relevant fields at create | `WebSettingsChromium`: `javascriptEnabled` → `view.create`, user agent → `view.setUserAgent`; every other setting is a structured refusal (`WIRE-ADDITIONS.md` §2) |
| `setWebViewClient` / `setWebChromeClient` | **guest** | stored, as before |
| `onVisibilityChanged`, `onWindowVisibilityChanged` | guest state + frame policy | the guest stops pulling frames while hidden; telling the engine is `WIRE-ADDITIONS.md` §1 |
| `getAccessibilityNodeProvider`, IME, keys, find-in-page, … | **engine, not landed** | structured `UnsupportedOperationException` naming the method — never a silent no-op |

`init(Map, boolean)` is implemented too (the constructor calls it right after
`ensureProviderCreated()`); `privateBrowsing=true` is refused for the same
"no wire message, no fake" reason.

## 1. What the ART run shows (the goal, measured)

`evidence/ev2-listener-wire.log` — the listener's own log, in arrival order:

```
[1] hello                       {"proto":1,"client":"acos-webview/0.4.0"}
[2] view.addJavascriptInterface (the fixture's bridge)
[3] view.create                 {"width":900,"height":700,"density":2.625}   ← first measure
[4] view.setViewport            {"width":900,"height":700,"density":2.625}   ← first layout
[5] view.loadUrl                {"url":"data:text/html;charset=utf-8,..."}
[6] view.setViewport            900x614 (the status TextView grew; ScrollView re-measured)
[7] view.setViewport            900x700
[8] input.setFocus              {"focused":false}
[9] input.setFocus              {"focused":true}
[10] render                     {"format":"png"} → JSON + BLOB(16x16 png, seq 1)
verdict: pass (ordered hello, view.create, view.setViewport, view.loadUrl; geometry 900x700)
```

and the guest side (`evidence/ev2-listener-logcat.txt`):

```
I/ACOSWvProvider: create(WebViewDelegate present)
I/WVProbe: WebView created: ...ProbeWebView{... VFED..... ......I. 0,0-0,0}
I/WVProbe: settings ok: jsEnabled=true
I/WVProbe: clients set
I/ACOSWvProvider: view.create view=1 900x700 density=2.625
I/ACOSWvProvider: view.setViewport view=1 900x700 density=2.625
I/WVProbe: laid out 900x700
I/ACOSWvProvider: view.loadUrl view=1 url=data:text/html;charset=utf-8,...
I/WVProbe: loadUrl issued
I/ACOSWvProvider: onDraw 900x700 dirty=true frame=none renderable=true
I/ACOSWvProvider: onDraw 900x700 frame=none -> render #1
I/ACOSWvProvider: frame decoded 16x16 seq=1 bytes=82
I/ACOSWvProvider: onDraw 900x700 dirty=false frame=present renderable=true
```

So: constructed, attached, measured, laid out at the View's real size, drawn,
navigated — and `onDraw` pulls a frame per `WIRE.md` §4.2 (one `render`, its JSON
response *and* its BLOB, decoded, then painted on the next draw).

`verify: pass`, 7 steps, guest exit 0 (`evidence/ev2-listener-verdict.txt`).

The listener is a **test harness** (`evidence/minimal-wire-listener.py`): WIRE v1
framing only, no engine, no Chromium, no pages. It never ships in the provider;
the host broker (`crates/acos-webview`, branch `wvb`) remains the production peer.

## 2. The negative control (arm 1) still fails honestly

`evidence/ev1-no-listener-verdict.txt` / `ev1-no-listener-logcat.txt`: with
nothing listening at `/run/acos/webview.sock` the provider reports

```
E/ACOSWvProvider: ACOS WebView engine unavailable: /run/acos/webview.sock:
    java.io.IOException: No such file or directory
I/WVProbe: WebView FAILED: java.lang.IllegalStateException: ACOS WebView engine
    unavailable (ENGINE_UNAVAILABLE) ... at AcosSessionSocket.open(...)
```

and the scenario's `assert_text "WebView created:"` fails (`status: fail`,
`ASSERTION_FAILED`). The pass arm cannot be reached without a real listener, so
the pair is a true A/B, not two passes.

## 3. Three real defects the walk exposed (each fixed at the cause)

1. **`WebView.setLayoutParams` is a pure provider delegate.** The image dex shows
   *no* `super` call, so `View.setLayoutParams`'s bookkeeping never runs and the
   parent reads `getLayoutParams() == null`:
   `LOOPER_THREW: NPE ... LinearLayout$LayoutParams.weight on a null object
   reference in measureVertical` (measured, first run of this branch). The
   provider is the View's own setter here, so it performs the same three steps
   (`mLayoutParams`, `requestLayout()`), and fails loudly with
   `IllegalStateException` if it cannot.
2. **`ViewGroup`'s constructor sets `WILL_NOT_DRAW`, and it runs *after*
   `View.<init>`.** Clearing the flag in `attach()` (i.e. from `View.<init>`'s
   `setOverScrollMode`, which creates the provider) is silently overwritten, so
   `onDraw` never ran (measured: no `onDraw` log, no `render` on the wire). It is
   cleared in `init()` instead — the framework's last provider call in the
   constructor. Guest scrollbars are disabled at the same point (the engine draws
   its own), which is also what keeps `View.onDrawScrollBars` from asking for
   engine scroll geometry the wire cannot provide.
3. **`render`'s BLOB was dropped by the guest transport.** `AcosWebViewTransport`
   offered the `render` JSON response into a 1-slot queue and then tried to
   re-offer it *with* the BLOB: either the BLOB was discarded (queue full) or the
   request had already left `pending`. `WIRE.md` §1/§6 make `render` the one op
   that is two frames, so the response is now **held** until its BLOB arrives
   (`Pending.hold/release`), and an error response completes immediately. The
   evidence above (`frame decoded 16x16 seq=1`) is that path working; before the
   fix the listener still answered, and the guest still painted nothing.

`compute*ScrollRange/Offset/Extent` (the next thing `View.draw` asks for, measured
as a `LOOPER_THREW` on the first attempt) answer from the guest's own geometry
(viewport size, guest scroll offsets) instead of throwing, and the missing
engine-side truth — content size, scroll position — is written up as a WIRE v1
proposal (`WIRE-ADDITIONS.md` §4).

## 4. WIRE v1: nothing was needed, five things are proposed

The goal asks first whether WIRE v1 already has the messages. It does:
`view.create`, `view.setViewport`, `view.loadUrl`, `view.loadData`,
`input.setFocus`, `input.touch`, `view.setUserAgent`, `render`, `view.destroy`,
`view.reload`, `view.stop`, `view.evaluateJavascript`,
`view.addJavascriptInterface` cover the whole walk. `results/handoff/WV5a/
WIRE-ADDITIONS.md` lists what the *rest* of the surface would need
(`view.setVisibility`, `view.settings`, a per-view profile, engine scroll
position, `ax`→`AccessibilityNodeProvider` + IME focus); branch `wvb` is
untouched.

## 5. Scenario

`results/handoff/WV5a/webview-probe-provider.yaml` — the WV4 scenario variant with
`expectation: pass` and five stage assertions (created / settings / clients /
laid out 900x700 / loadUrl issued), used with the test listener.
`results/handoff/WV5a/webview-probe-provider-no-listener.yaml` — the same probe
with `expectation: fail` + `ASSERTION_FAILED`, the negative control (arm 1).

The fixture (`fixtures/apps/webview-probe`) now issues its navigation from
`onSizeChanged` — the framework's own "the view has its real size" callback — so
`view.create` and `view.setViewport` precede `view.loadUrl` by construction, and
uses a `data:` URL (Chromium never network-fetches `data:`, `WIRE.md` §4.3).

## 6. Tests

* `cargo test -p acos-art --lib` → **159 passed, 0 failed, 1 ignored**
  (`evidence/host-tests-acos-art-lib.txt`), with `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`.
* `python3 tools/gen-webview-provider.py --check` → clean
  (`evidence/gen-webview-provider-check.txt`): the generated provider surface
  (including the new `WebSettingsChromium`, generated from
  `research/spike-wv/provider/WEBSETTINGS.md`) is current.
* Fails-before/passes-after: the *before* for the whole wall is WV4's
  `ev2-listener-logcat.txt` (`setOverScrollMode` UOE); for §3.1 the first run of
  this branch (`LinearLayout` NPE), for §3.2 the run without `init()`'s
  `setWillNotDraw(false)` (no `onDraw`, no `render`), for §3.3 the run whose
  listener answered `render` and whose guest painted nothing. Each "after" is
  `ev2-listener-wire.log` + `ev2-listener-logcat.txt`.

## 7. Commands used

```sh
# provider + probe (the generator runs inside the build)
ACOS_APP_ONLY="webview-probe webview-provider" tools/build-apps.sh

# test listener (test harness; WIRE v1 only)
mkdir -p .cache/wv5a-run/wv
python3 results/handoff/WV5a/evidence/minimal-wire-listener.py \
  $PWD/.cache/wv5a-run/wv/webview.sock $PWD/.cache/wv5a-run/listener.log \
  --verdict=$PWD/.cache/wv5a-run/listener-verdict.txt &

# arm 2 (listener)
ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts \
ACOS_RUNTIME_DIR=$PWD/.cache/wv5a-run ACOS_SESSION_SOCKET=$PWD/.cache/wv5a-run/session.sock \
ACOS_WEBVIEW_SOCKET_DIR=$PWD/.cache/wv5a-run/wv \
ACOS_ART_BIN=$PWD/.cache/wv5a-target/release/acos-art \
tools/art-runtime/run-exclusive.sh --task WV5a -- tools/guarded.sh --mem 8G --time 700 -- \
  ./.cache/wv5a-target/release/acos verify .cache/wv5a-run/scenario-listener.yaml \
  --engine art --artifacts .cache/wv5a-run/ev2

# arm 1 (no listener; drop ACOS_WEBVIEW_SOCKET_DIR and the listener)
  ... ./.cache/wv5a-target/release/acos verify .cache/wv5a-run/scenario-no-listener.yaml \
  --engine art --artifacts .cache/wv5a-run/ev1
```

Builds: `CARGO_TARGET_DIR=$PWD/.cache/wv5a-target CARGO_PROFILE_DEV_DEBUG=0
CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo build --release --offline
-p acos-cli` and `-p acos-art --bin acos-art`.

## 8. What remains

* The host broker (WVB) — this branch still runs against a test listener only.
* `WIRE-ADDITIONS.md` §1–§5 (visibility, settings after create, profiles, engine
  scroll truth, `ax`→a11y + IME) — each one is a broker+guest pair.
* Keys/find-in-page/zoom/`capturePicture`-class surface stays a structured
  refusal until its wire message exists.
