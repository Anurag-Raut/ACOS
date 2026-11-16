# WV5a — WIRE v1 additions the guest provider needs (proposal; the `wvb` branch is unchanged)

Wall 9 is the guest-provider surface the framework's `new WebView(context)` and
first measure/layout/draw walk reaches. Everything that needs the engine is a
**WIRE v1 request that already exists** (`view.create`, `view.setViewport`,
`view.loadUrl`, `view.loadData`, `input.touch`, `input.setFocus`,
`input.setUserAgent`, `render`, `view.destroy`, `view.reload`, `view.stop`,
`view.evaluateJavascript`, `view.addJavascriptInterface`), so nothing below is
required for the wall-9 goal. These are the gaps found while walking the surface:
each one is a message WIRE v1 does not have, and each one is implemented as an
honest refusal (a structured `UnsupportedOperationException` naming the method)
rather than a silent no-op that would report success it cannot deliver.

Normative document stays `results/handoff/WVB/WIRE.md` on branch `wvb`; this file
only *proposes*. Additive ops do not bump `proto` (§7), and a broker that does not
know an op answers `UNSUPPORTED` (§2), so a guest that sent one of these before
the broker landed would get a clean error.

## 1. `view.setVisibility` — tell the engine the View's visibility

*Guest side:* `ViewDelegate.onVisibilityChanged` / `onWindowVisibilityChanged`
(`WebViewChromiumCore.onVisibilityChanged`).
*Today:* the visibility is guest state; an invisible WebView simply stops asking
for frames (`renderable()`), and the engine is never told. That is honest but
incomplete: Chromium maps a hidden WebView onto `Page.setLifecycleEventsEnabled`
/ `Emulation.setPageVisibilityState` and stops the renderer's compositor, so
leaving the engine running is wasted work, and a page that polls
`document.visibilityState` sees `visible` while the View is `GONE`.

```json
{"id":"g12","op":"view.setVisibility","view":1,"visible":false,"windowVisible":true}
    -> {"id":"g12","ok":true,"result":{"view":1,"visible":false,"windowVisible":true}}
```

## 2. `view.settings` — WebSettings changes after `view.create`

*Guest side:* `WebSettingsChromium` (`tools/gen-webview-provider.py`,
`research/spike-wv/provider/WEBSETTINGS.md`). WIRE v1 has exactly two settings
fields: `view.create.javascriptEnabled` and `view.setUserAgent`.
*Today:* `setJavaScriptEnabled` before the engine view exists is applied by
`view.create`; changing it **after** the create is refused (it would otherwise be
a setting the app thinks it changed and the page never sees). The rest of
`android.webkit.WebSettings`'s 92 abstract methods have no message at all and are
refused, naming themselves.

```json
{"id":"g13","op":"view.settings","view":1,
 "settings":{"javascriptEnabled":true,"loadsImagesAutomatically":false,
             "cacheMode":1,"textZoom":120,"userAgent":"…","domStorageEnabled":true}}
    -> {"id":"g13","ok":true,"result":{"view":1,"applied":["javascriptEnabled"],
                                       "rejected":["cacheMode"]}}
```

`rejected` is the important half: an op that silently ignored `cacheMode` would
be the false success this track forbids. A settings key the broker does not
implement is either applied or reported in `rejected`, never dropped.

Fields worth adding first, in the order real pages need them:
`javascriptEnabled`, `userAgent`, `loadsImagesAutomatically`, `blockNetworkImage`,
`blockNetworkLoads`, `domStorageEnabled`, `databaseEnabled`, `cacheMode`,
`mixedContentMode`, `textZoom`, `defaultFontSize`/`defaultFixedFontSize`,
`useWideViewPort`, `loadWithOverviewMode`, `layoutAlgorithm`, `supportZoom`,
`builtInZoomControls`, `mediaPlaybackRequiresUserGesture`.

Mapping (needs the engine, host side): `cacheMode`→`Network.setCacheDisabled` +
`Page.setBypassCSP`-adjacent flags, `textZoom`→`Emulation.setPageScaleFactor`
or a CSS zoom, `loadsImagesAutomatically`→`Emulation.setAutoDarkModeOverride`-
style settings via `Page.setInterceptFileChooserDialog`… i.e. a per-view
`Emulation`/`Page` settings bundle, resolved host-side against the pinned
133-era protocol.

## 3. private browsing / per-view profile

*Guest side:* `WebViewProvider.init(Map, boolean)`: `privateBrowsing=true` is
refused (the hidden `WebView(Context, AttributeSet, int, int, Map, boolean)`
constructor is the only caller). v1 has one engine profile per session, so a
shared-cache WebView cannot be honestly presented as private.

```json
{"id":"g14","op":"view.create","view":1,"width":900,"height":700,"density":2.625,
 "profile":"incognito"}
    -> {"id":"g14","ok":true,"result":{"view":1,"width":900,"height":700,"density":2.625,
                                       "profile":"incognito"}}
```

(`profile` is additive on `view.create`; the broker answers `rejected`-style
`UNSUPPORTED` today via the unknown-op path only if `profile` is set and it
cannot honour it — that needs a documented field rule, not a silent ignore.)

## 4. engine scroll position (guest `computeScroll` / `scrollTo`)

*Guest side:* `ScrollDelegate.computeScroll` is a no-op today: the guest has no
scroller to advance, and a WebView's scrolling is the engine's (`flingScroll`,
`pageDown`, `pageUp` are refused as unsupported). `View.scrollTo(x, y)` is not
overridden by `WebView`, so an app that calls it scrolls the *guest canvas* while
the page does not move — a real fidelity gap that no wire op can currently close.

```json
{"id":"g15","op":"view.scrollTo","view":1,"x":0,"y":840}
    -> {"id":"g15","ok":true,"result":{"view":1,"x":0,"y":840,"contentHeight":5200}}

{"id":"h9","event":"view.scrolled","view":1,"x":0,"y":840,"contentHeight":5200}
```

plus `contentHeight`/`contentWidth`/`scrollX`/`scrollY` in the `render` response,
so `getContentHeight()`, `computeVerticalScrollRange/Offset/Extent` and
`ScrollDelegate.computeScroll` can be answered from engine truth instead of being
refused. With `view.scrolled` the guest can keep `View.scrollTo` honest by
forwarding it to the engine and copying the engine's answer back.

## 5. accessibility tree and text-field state (`ax`, IME)

*Guest side:* `ViewDelegate.getAccessibilityNodeProvider()` refuses (WIRE v1 §4.4
`ax` exists, but mapping an `AccessibilityNodeProvider` onto it has not landed —
returning `null` would claim the page has no virtual children, a false answer
about a WebView with content). `onCheckIsTextEditor()` answers `false` and
`onCreateInputConnection` answers `null` for the same reason: v1 has no message
reporting that the *engine* has a focused editable field, so the guest cannot
know when a real IME connection is required.

Proposed: `{"event":"view.focusChanged","view":1,"focused":true,
"editable":{"focused":true,"type":"text","inputType":1}}`, and the `ax` response
(§4.4) already carries `role:"textField"`/`editable` — enough to build both the
`AccessibilityNodeProvider` and the IME answer, so this needs **no new op**, only
a guest-side mapping plus the focus event above.
