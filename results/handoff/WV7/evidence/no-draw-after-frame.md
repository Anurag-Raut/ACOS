# (c) second cause: the guest never paints the frame it decoded

Evidence: `logcat-nodraw.txt` (run ev9, the same provider build as the passing
run) and `logcat-pass.txt`.

```
onDraw view=1 900x560 dirty=true frame=none renderable=true attached=true
onDraw view=2 900x560 dirty=true frame=none renderable=true attached=true
onDraw view=1 ... frame=none ...            (layout/text invalidations)
onDraw view=2 ... frame=none ...
frame decoded view=2 343x213 seq=1 ... attached=true handler=Handler (android.view.ViewRootImpl$ViewRootHandler)
                                            <- and after this line: no onDraw at all for either View
```

So, after `setFrame` -> `invalidate()` the View never draws again:

* `View.postInvalidateOnAnimation()`, `View.postInvalidate()` and
  `view.post(() -> view.invalidate())` were all tried (all three are in the
  provider's `invalidate()` now); none produces a traversal in the ART session.
* The View is attached (`isAttachedToWindow() == true`) and has the real
  `ViewRootImpl$ViewRootHandler`, so the post itself lands on the right handler.
* Host-driven draws do happen: the same session's tap step changes the captured
  frame (`pixels_changed: true` in `steps/10-tap.json`), i.e. an input-driven
  traversal works, an invalidation-driven one does not.

A magenta 40x40 `drawRect` from the same `onDraw` **was** visible in the capture
when a draw ran (1425 magenta pixels at display (21,700) in run ev5), so the
child's drawing does reach the C1 window buffer; the missing piece is the
traversal, not the compositing.

Where it lives: the guest's invalidate -> `Choreographer` -> `scheduleVsync` ->
host frame clock -> `doTraversal` path (`acos-services/src/display_events.rs`
request-driven clock; `acos-art`'s session draws on `nSyncAndDrawFrame`). That is
guest-framework/vsync plumbing, not the WebView module or the wire.
