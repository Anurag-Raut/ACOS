# TOUCHDISP — where the touch actually goes (and why "the listener never ran")

Branch `touchdisp` (base `rel-030` @ `0e3719bf`, which includes REL3H2).
Fixture: `fixtures/apps/touch-probe` (`MainActivity`, `ProbeView`,
`GestureProbeView`). Artifacts in this directory.

## 1. The measurement that REL3H2 could not make

REL3H2 recorded: *"the ART guest's dispatch path acks `unhandled` without
consulting a non-clickable view's `OnTouchListener`: it looks like a hit test on
clickable nodes rather than a real `ViewGroup.dispatchTouchEvent` chain"*, and on
that basis removed the posted-side-effect fixture as "blocked by an emulation
gap".

The same shape, instrumented from inside the app, shows the opposite. The app
logs at four points (`Activity.dispatchTouchEvent`, a custom View's
`dispatchTouchEvent`, its listener, its `onTouchEvent`), taps the non-clickable
`label` and then the non-clickable custom `probe`:

<!-- dispatch-probe-logcat.txt (verbatim, in order; `AcosTouchProbe` lines only) -->
```text
activity.dispatchTouchEvent DOWN x=540.0 y=537.0 src=0x1002
root.onTouch DOWN x=540.0 y=262.0
activity.dispatchTouchEvent UP x=540.0 y=537.0 src=0x1002
activity.dispatchTouchEvent DOWN x=540.0 y=905.0 src=0x1002
probe.dispatchTouchEvent DOWN x=498.0 y=210.0 raw=540.0,905.0 src=0x1002 flags=0x0 enabled=true clickable=false
probe.onTouch DOWN x=498.0 y=210.0 t=24478226
probe.onTouchEvent DOWN x=498.0 y=210.0
root.onTouch DOWN x=540.0 y=630.0
activity.dispatchTouchEvent UP x=540.0 y=905.0 src=0x1002
probe.posted setText(touched-delayed)
```

So on the real ART guest:

* the DOWN goes `ViewRootImpl` → `DecorView` → `Activity` → `ViewGroup` hit test →
  the **deepest non-clickable child** → that view's `dispatchTouchEvent` → its
  `mOnTouchListener.onTouch` → its `onTouchEvent` → the ancestor `ViewGroup`'s own
  `dispatchTouchEvent` fallback (`root.onTouch`): the real chain, in the real
  order;
* the coordinates are the window's local pixels at each level (`540,905` at the
  activity, `498,210` inside `probe` after the ancestors' offsets), the source is
  `0x1002` (`SOURCE_TOUCHSCREEN`), the event is a touch, and the
  view is `enabled=true clickable=false`;
* **the UP reaches the `Activity` but not the view tree.** That is not a gap. AOSP
  `ViewGroup.dispatchTouchEvent` (`base/core/java/android/view/ViewGroup.java:2665`)
  says it in the source:

  > `// There are no touch targets and this action is not an initial down`
  > `// so this view group continues to intercept touches.`

  A DOWN nobody consumed never creates a `TouchTarget`, so for the UP the
  `DecorView`'s own `ViewGroup` sets `intercepted = true`, does not hit-test its
  children, and dispatches only to *itself*. The UP reaches `Activity` /
  `PhoneWindow` / the decor's own listener and stops there. `View.onTouchEvent`
  returning false means "I am not this gesture's target", and Android honours it.

`View.dispatchTouchEvent` (`View.java:16253`) is what makes the listener run for
*any* ENABLED view, clickable or not:

```java
ListenerInfo li = mListenerInfo;
if (li != null && li.mOnTouchListener != null && (mViewFlags & ENABLED_MASK) == ENABLED) {
    handled = li.mOnTouchListener.onTouch(this, event);
}
```

**Conclusion: there is no dispatch divergence to fix.** The guest runs the real
`ViewRootImpl`/`DecorView`/`ViewGroup`/`View` chain. The REL3H2 fixture's listener
only ever acted on `ACTION_UP` (the only action a "tap" has in a click listener's
mental model); because it returned false it was never a touch target, so the UP
never came and no log line and no posted `setText` appeared. The emulation was
faithful; the fixture asked for an event Android does not deliver.

## 2. What that unblocks

Both behaviours REL3H2 left untested are now pinned on a real guest:

| Case | Scenario | ART test |
|---|---|---|
| Non-clickable view, listener returns false, **posts** `setText` 100 ms out; `expect_change: true` must capture the change (not bind the pre-state) | `scenarios/touchdisp-posted-side-effect.yaml` | `touchdisp_dispatch::a_posted_side_effect_of_an_unhandled_tap_is_captured_not_the_pre_state` |
| Honest failure twin: the same rule on a gesture that really changes nothing still fails `expect_change` | `scenarios/rel3h-unhandled-input-expect-change.yaml` | `touchdisp_dispatch::an_unhandled_tap_that_changes_nothing_still_fails_expect_change` |
| `GestureDetector` on a non-clickable view: a held long press reaches `View.onTouchEvent` and changes state | `scenarios/touchdisp-gesture.yaml` | `touchdisp_dispatch::a_long_press_on_a_non_clickable_gesture_surface_changes_state` |

### The posted-side-effect step, measured (`published/step-01-tap.json`)

```json
"input":  { "handled": false,
            "handled_evidence": { "published": 2, "acked": 2, "unhandled": [3, 4] } }
"settle": { "mode": "strict", "settled": true, "bound": "strict-event",
            "ack_ms": 5, "first_change_ms": 127, "frame_ms": 166,
            "pixels_changed": true, "waited_ms": 219 }
```

* the window acknowledged **both** events `handled = false` (the unhandled-input
  rule is in play), yet the step did **not** settle on the pre-state:
  `bound: strict-event`, `first_change_ms = 127 ms` is *after* `ack_ms = 5 ms`, and
  the bound tree (`published/tree-3.json`) carries `status = "touched-delayed"`,
  the listener's posted text — not `idle`;
* the negative control does the opposite on purpose: `control/step-01-tap.json`
  has `settle.bound: "unhandled-input"`, `pixels_changed: false`, and the
  `expect_change` assertion fails with `settled frame did not change from the
  pre-action frame`. Both rules keep their meaning.

### The gesture step, measured (`gesture/step-01-swipe.json`)

```json
"input":  { "handled_evidence": { "published": 44, "acked": 44,
                                  "unhandled": [4, 5, ... 46] } }   // DOWN (seq 3) handled
"settle": { "mode": "strict", "settled": true, "bound": "strict-event",
            "ack_ms": 700, "first_change_ms": 780, "pixels_changed": true }
```

44 events (DOWN + paced MOVEs + UP) were published and every one acked; the
`GestureDetector` in the non-clickable view logged `onDown` right after the DOWN
and `onLongPress` at ~575 ms into the hold (`gesture/logcat.txt`), and wrote
`status = long-pressed`. The DOWN is `handled = true` (the detector claimed the
stream) and the MOVEs/UP are `handled = false` (`SimpleOnGestureListener` does not
consume them) — which is exactly how the same view behaves on a device.

## 3. Residual limit (not fixed, by design of the rule)

The unhandled-input shortcut and the normal strict rule both bound the *newest*
draw. The guest's probe reports pending framework work only when the next
main-Looper message is due **inside its horizon**, so an app that posts a side
effect far later than the settle window *and* draws something in between can
still be reported as settled before the late side effect lands. That is the
limit REL3H2 documented; with a 100 ms `postDelayed` and no other draw the probe
takes no sample in between, so the change is captured (measured above). Widening
the horizon would make every tap on an app with a periodic housekeeping post
wait for it; that is an owner decision, not a dispatch bug.

## 4. Why no code change was needed in the dispatch path

Every element the brief listed as a divergence candidate was measured and is
right at the guest: coordinates are window-local and per-view transformed, the
window has focus and touch mode (the host publishes `FOCUS`/`TOUCH_MODE` on the
real `InputChannel`), the source/action/flag fields are the touchscreen ones, the
input stage chain reaches `mView.dispatchPointerEvent`, and nothing answers
host-side. The only thing that was wrong was the *evidence* about it — the
fixture listened for an event Android does not deliver to a non-consuming view.
