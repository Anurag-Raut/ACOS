# Item 3 — every scenario in scenarios/ via `acos verify --engine art` (RC f162b2d9, release)

| scenario | expected | got | error code | steps | verdict path | as declared |
|---|---|---|---|---|---|---|
| `back-layers-ime.yaml` | pass | pass |  | 22 | `04-scenarios/back-layers-ime/artifacts/verdict.json` | OK |
| `back-layers.yaml` | pass | pass |  | 22 | `04-scenarios/back-layers/artifacts/verdict.json` | OK |
| `counter.yaml` | pass | pass |  | 24 | `04-scenarios/counter/artifacts/verdict.json` | OK |
| `finish-start.yaml` | pass | pass |  | 7 | `04-scenarios/finish-start/artifacts/verdict.json` | OK |
| `form-validation.yaml` | pass | pass |  | 28 | `04-scenarios/form-validation/artifacts/verdict.json` | OK |
| `gaten-backstack.yaml` | pass | pass |  | 25 | `04-scenarios/gaten-backstack/artifacts/verdict.json` | OK |
| `gaten-content-welcome.yaml` | pass | pass |  | 4 | `04-scenarios/gaten-content-welcome/artifacts/verdict.json` | OK |
| `gaten-home.yaml` | pass | pass |  | 14 | `04-scenarios/gaten-home/artifacts/verdict.json` | OK |
| `gaten-kanji.yaml` | pass | pass |  | 33 | `04-scenarios/gaten-kanji/artifacts/verdict.json` | OK |
| `gaten-lesson.yaml` | pass | timeout | TIMEOUT | 26 | `04-scenarios/gaten-lesson/artifacts/verdict.json` | **NOT OK** |
| `gaten-recognition.yaml` | pass | pass |  | 33 | `04-scenarios/gaten-recognition/artifacts/verdict.json` | OK |
| `gaten-search.yaml` | pass | pass |  | 22 | `04-scenarios/gaten-search/artifacts/verdict.json` | OK |
| `gaten-settings.yaml` | pass | pass |  | 33 | `04-scenarios/gaten-settings/artifacts/verdict.json` | OK |
| `gaten-support.yaml` | pass | pass |  | 27 | `04-scenarios/gaten-support/artifacts/verdict.json` | OK |
| `gaten-tabs.yaml` | pass | pass |  | 22 | `04-scenarios/gaten-tabs/artifacts/verdict.json` | OK |
| `hello-jni.yaml` | pass | pass |  | 13 | `04-scenarios/hello-jni/artifacts/verdict.json` | OK |
| `hello-views.yaml` | pass | timeout | TIMEOUT | 24 | `04-scenarios/hello-views/artifacts/verdict.json` | **NOT OK** |
| `multi-activity-finish.yaml` | pass | pass |  | 19 | `04-scenarios/multi-activity-finish/artifacts/verdict.json` | OK |
| `multi-activity.yaml` | pass | timeout | TIMEOUT | 12 | `04-scenarios/multi-activity/artifacts/verdict.json` | **NOT OK** |
| `negative/counter-wrong-count.yaml` | fail | fail | ASSERTION_FAILED | 3 | `04-scenarios/negative__counter-wrong-count/artifacts/verdict.json` | OK |
| `negative/finish-start-wrong-home.yaml` | fail | fail | ASSERTION_FAILED | 4 | `04-scenarios/negative__finish-start-wrong-home/artifacts/verdict.json` | OK |
| `negative/form-wrong-error.yaml` | fail | fail | ELEMENT_OFFSCREEN | 3 | `04-scenarios/negative__form-wrong-error/artifacts/verdict.json` | OK |
| `negative/gaten-backstack-wrong.yaml` | fail | fail | ASSERTION_FAILED | 24 | `04-scenarios/negative__gaten-backstack-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-content-assert-screenshot.yaml` | fail `ASSERTION_FAILED/content_settle_pixel` | fail | ASSERTION_FAILED | 2 | `04-scenarios/negative__gaten-content-assert-screenshot/artifacts/verdict.json` | OK |
| `negative/gaten-content-pixel.yaml` | fail `ASSERTION_FAILED/content_settle_pixel` | fail | ASSERTION_FAILED | 3 | `04-scenarios/negative__gaten-content-pixel/artifacts/verdict.json` | OK |
| `negative/gaten-content-selector-bounds.yaml` | fail `ASSERTION_FAILED/content_settle_geometry` | fail | ASSERTION_FAILED | 2 | `04-scenarios/negative__gaten-content-selector-bounds/artifacts/verdict.json` | OK |
| `negative/gaten-content-welcome-wrong.yaml` | fail `ASSERTION_FAILED/None` | fail | ASSERTION_FAILED | 2 | `04-scenarios/negative__gaten-content-welcome-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-content-window-impossible.yaml` | non_pass `TIMEOUT/content-not-stable` | timeout | TIMEOUT | 3 | `04-scenarios/negative__gaten-content-window-impossible/artifacts/verdict.json` | OK |
| `negative/gaten-home-wrong.yaml` | fail | fail | ASSERTION_FAILED | 8 | `04-scenarios/negative__gaten-home-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-kanji-wrong.yaml` | fail | fail | ASSERTION_FAILED | 20 | `04-scenarios/negative__gaten-kanji-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-lesson-wrong.yaml` | fail | fail | ASSERTION_FAILED | 11 | `04-scenarios/negative__gaten-lesson-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-recognition-wrong.yaml` | fail | fail | ASSERTION_FAILED | 29 | `04-scenarios/negative__gaten-recognition-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-search-wrong.yaml` | fail | fail | ASSERTION_FAILED | 17 | `04-scenarios/negative__gaten-search-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-settings-wrong.yaml` | fail | fail | ASSERTION_FAILED | 13 | `04-scenarios/negative__gaten-settings-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-support-wrong.yaml` | fail | fail | ASSERTION_FAILED | 14 | `04-scenarios/negative__gaten-support-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-tabs-wrong.yaml` | fail | fail | ASSERTION_FAILED | 8 | `04-scenarios/negative__gaten-tabs-wrong/artifacts/verdict.json` | OK |
| `negative/hello-views-back-text-gone.yaml` | fail | fail | ELEMENT_NOT_FOUND | 4 | `04-scenarios/negative__hello-views-back-text-gone/artifacts/verdict.json` | OK |
| `negative/missing-element.yaml` | fail | fail | ELEMENT_NOT_FOUND | 3 | `04-scenarios/negative__missing-element/artifacts/verdict.json` | OK |
| `negative/missing-jni-line.yaml` | fail | fail | ASSERTION_FAILED | 3 | `04-scenarios/negative__missing-jni-line/artifacts/verdict.json` | OK |
| `negative/multi-back-save-state.yaml` | fail | fail | ASSERTION_FAILED | 4 | `04-scenarios/negative__multi-back-save-state/artifacts/verdict.json` | OK |
| `negative/multi-wrong-after-back.yaml` | fail | timeout | TIMEOUT | 7 | `04-scenarios/negative__multi-wrong-after-back/artifacts/verdict.json` | **NOT OK** |
| `negative/multi-wrong-message.yaml` | fail | fail | ASSERTION_FAILED | 3 | `04-scenarios/negative__multi-wrong-message/artifacts/verdict.json` | OK |
| `negative/offscreen-pager-visible.yaml` | fail | fail | ELEMENT_OFFSCREEN | 3 | `04-scenarios/negative__offscreen-pager-visible/artifacts/verdict.json` | OK |
| `negative/offscreen-pager-wait.yaml` | fail | timeout | TIMEOUT | 3 | `04-scenarios/negative__offscreen-pager-wait/artifacts/verdict.json` | **NOT OK** |
| `negative/rot1-rotate-wrong.yaml` | fail | fail | ASSERTION_FAILED | 5 | `04-scenarios/negative__rot1-rotate-wrong/artifacts/verdict.json` | OK |
| `negative/se1-expect-change-noop.yaml` | fail | fail | ASSERTION_FAILED | 3 | `04-scenarios/negative__se1-expect-change-noop/artifacts/verdict.json` | OK |
| `negative/todo-missing-item.yaml` | fail | fail | ELEMENT_NOT_FOUND | 3 | `04-scenarios/negative__todo-missing-item/artifacts/verdict.json` | OK |
| `negative/w44-hostile-forest-trusted.yaml` | fail | fail | ELEMENT_NOT_FOUND | 2 | `04-scenarios/negative__w44-hostile-forest-trusted/artifacts/verdict.json` | OK |
| `negative/wrong-bounds.yaml` | fail | fail | ASSERTION_FAILED | 3 | `04-scenarios/negative__wrong-bounds/artifacts/verdict.json` | OK |
| `negative/wrong-lifecycle-order.yaml` | fail | fail | ASSERTION_FAILED | 3 | `04-scenarios/negative__wrong-lifecycle-order/artifacts/verdict.json` | OK |
| `negative/wrong-text.yaml` | fail | fail | ASSERTION_FAILED | 3 | `04-scenarios/negative__wrong-text/artifacts/verdict.json` | OK |
| `p11-activity.yaml` | pass | pass |  | 7 | `04-scenarios/p11-activity/artifacts/verdict.json` | OK |
| `p11-anim.yaml` | pass | pass |  | 4 | `04-scenarios/p11-anim/artifacts/verdict.json` | OK |
| `p11-background.yaml` | pass | pass |  | 5 | `04-scenarios/p11-background/artifacts/verdict.json` | OK |
| `p11-chain.yaml` | pass | pass |  | 4 | `04-scenarios/p11-chain/artifacts/verdict.json` | OK |
| `p11-delay1000.yaml` | pass | pass |  | 5 | `04-scenarios/p11-delay1000/artifacts/verdict.json` | OK |
| `p11-delay200.yaml` | pass | pass |  | 4 | `04-scenarios/p11-delay200/artifacts/verdict.json` | OK |
| `p11-delay40.yaml` | pass | pass |  | 4 | `04-scenarios/p11-delay40/artifacts/verdict.json` | OK |
| `p11-dialog.yaml` | pass | pass |  | 6 | `04-scenarios/p11-dialog/artifacts/verdict.json` | OK |
| `p11-layout-transition.yaml` | pass | pass |  | 4 | `04-scenarios/p11-layout-transition/artifacts/verdict.json` | OK |
| `p11-list-smooth.yaml` | pass | pass |  | 4 | `04-scenarios/p11-list-smooth/artifacts/verdict.json` | OK |
| `p11-progress-timeout.yaml` | non_pass | timeout | TIMEOUT | 3 | `04-scenarios/p11-progress-timeout/artifacts/verdict.json` | OK |
| `p11-rapid-taps.yaml` | pass | pass |  | 13 | `04-scenarios/p11-rapid-taps/artifacts/verdict.json` | OK |
| `p12-progress-anim-off.yaml` | pass | pass |  | 6 | `04-scenarios/p12-progress-anim-off/artifacts/verdict.json` | OK |
| `rel3a-content-pixel.yaml` | non_pass | fail | ASSERTION_FAILED | 5 | `04-scenarios/rel3a-content-pixel/artifacts/verdict.json` | OK |
| `rel3a-content-settle.yaml` | pass | pass |  | 6 | `04-scenarios/rel3a-content-settle/artifacts/verdict.json` | OK |
| `rel3a-content-sticky.yaml` | non_pass | timeout | TIMEOUT | 5 | `04-scenarios/rel3a-content-sticky/artifacts/verdict.json` | OK |
| `rel3h2-expect-handled-false.yaml` | pass | pass |  | 4 | `04-scenarios/rel3h2-expect-handled-false/artifacts/verdict.json` | OK |
| `rel3h2-expect-handled-true.yaml` | fail `INPUT_NOT_HANDLED/expect_handled` | fail | INPUT_NOT_HANDLED | 2 | `04-scenarios/rel3h2-expect-handled-true/artifacts/verdict.json` | OK |
| `rel3h-unhandled-input-expect-change.yaml` | fail | fail | ASSERTION_FAILED | 2 | `04-scenarios/rel3h-unhandled-input-expect-change/artifacts/verdict.json` | OK |
| `rel3h-unhandled-input.yaml` | pass | pass |  | 4 | `04-scenarios/rel3h-unhandled-input/artifacts/verdict.json` | OK |
| `rel3p-pms-parcel.yaml` | pass | pass |  | 8 | `04-scenarios/rel3p-pms-parcel/artifacts/verdict.json` | OK |
| `rel3s-gaten-scroll.yaml` | pass | pass |  | 12 | `04-scenarios/rel3s-gaten-scroll/artifacts/verdict.json` | OK |
| `rot1-rotate.yaml` | pass | pass |  | 18 | `04-scenarios/rot1-rotate/artifacts/verdict.json` | OK |
| `s8-hostile-forged.yaml` | pass | pass |  | 2 | `04-scenarios/s8-hostile-forged/artifacts/verdict.json` | OK |
| `s8-hostile-region.yaml` | pass | timeout | TIMEOUT | 2 | `04-scenarios/s8-hostile-region/artifacts/verdict.json` | **NOT OK** |
| `s8-hostile-untrusted.yaml` | pass | fail | INSUFFICIENT_HOST_EVIDENCE | 2 | `04-scenarios/s8-hostile-untrusted/artifacts/verdict.json` | **NOT OK** |
| `se1-expect-change-tap.yaml` | pass | pass |  | 5 | `04-scenarios/se1-expect-change-tap/artifacts/verdict.json` | OK |
| `suite/T01-install-good.yaml` | pass | pass |  | 5 | `04-scenarios/suite__T01-install-good/artifacts/verdict.json` | OK |
| `suite/T01-install-wrong.yaml` | fail | fail | ASSERTION_FAILED | 3 | `04-scenarios/suite__T01-install-wrong/artifacts/verdict.json` | OK |
| `suite/T02-state-good.yaml` | pass | pass |  | 11 | `04-scenarios/suite__T02-state-good/artifacts/verdict.json` | OK |
| `suite/T02-state-wrong.yaml` | fail | fail | ASSERTION_FAILED | 4 | `04-scenarios/suite__T02-state-wrong/artifacts/verdict.json` | OK |
| `suite/T03-render-good.yaml` | pass | pass |  | 5 | `04-scenarios/suite__T03-render-good/artifacts/verdict.json` | OK |
| `suite/T03-render-wrong.yaml` | fail | fail | ASSERTION_FAILED | 3 | `04-scenarios/suite__T03-render-wrong/artifacts/verdict.json` | OK |
| `suite/T04-text-good.yaml` | pass | pass |  | 5 | `04-scenarios/suite__T04-text-good/artifacts/verdict.json` | OK |
| `suite/T04-text-wrong.yaml` | fail | fail | ASSERTION_FAILED | 2 | `04-scenarios/suite__T04-text-wrong/artifacts/verdict.json` | OK |
| `suite/T06-nav-good.yaml` | pass | pass |  | 9 | `04-scenarios/suite__T06-nav-good/artifacts/verdict.json` | OK |
| `suite/T06-nav-wrong.yaml` | fail | fail | ASSERTION_FAILED | 5 | `04-scenarios/suite__T06-nav-wrong/artifacts/verdict.json` | OK |
| `suite/T07-form-good.yaml` | pass | fail | ELEMENT_OFFSCREEN | 9 | `04-scenarios/suite__T07-form-good/artifacts/verdict.json` | **NOT OK** |
| `suite/T07-form-wrong.yaml` | fail | fail | ASSERTION_FAILED | 8 | `04-scenarios/suite__T07-form-wrong/artifacts/verdict.json` | OK |
| `suite/T08-persist-good.yaml` | pass | pass |  | 12 | `04-scenarios/suite__T08-persist-good/artifacts/verdict.json` | OK |
| `suite/T08-persist-wrong.yaml` | fail | fail | ASSERTION_FAILED | 8 | `04-scenarios/suite__T08-persist-wrong/artifacts/verdict.json` | OK |
| `suite/T09-reset-good.yaml` | pass | pass |  | 8 | `04-scenarios/suite__T09-reset-good/artifacts/verdict.json` | OK |
| `suite/T09-reset-wrong.yaml` | fail | fail | ASSERTION_FAILED | 5 | `04-scenarios/suite__T09-reset-wrong/artifacts/verdict.json` | OK |
| `suite/T10-isolation-good.yaml` | pass | pass |  | 14 | `04-scenarios/suite__T10-isolation-good/artifacts/verdict.json` | OK |
| `suite/T10-isolation-wrong.yaml` | fail | fail | ELEMENT_NOT_FOUND | 6 | `04-scenarios/suite__T10-isolation-wrong/artifacts/verdict.json` | OK |
| `suite/T11-shared-good.yaml` | pass | pass |  | 9 | `04-scenarios/suite__T11-shared-good/artifacts/verdict.json` | OK |
| `suite/T11-shared-wrong.yaml` | fail | fail | ASSERTION_FAILED | 4 | `04-scenarios/suite__T11-shared-wrong/artifacts/verdict.json` | OK |
| `suite/T12-dialog-good.yaml` | pass | pass |  | 7 | `04-scenarios/suite__T12-dialog-good/artifacts/verdict.json` | OK |
| `suite/T12-dialog-wrong.yaml` | fail | fail | ASSERTION_FAILED | 3 | `04-scenarios/suite__T12-dialog-wrong/artifacts/verdict.json` | OK |
| `svg-viewbox.yaml` | pass | fail | ASSERTION_FAILED | 3 | `04-scenarios/svg-viewbox/artifacts/verdict.json` | **NOT OK** |
| `todo-list.yaml` | pass | pass |  | 29 | `04-scenarios/todo-list/artifacts/verdict.json` | OK |
| `touchdisp-dispatch-probe.yaml` | pass | pass |  | 4 | `04-scenarios/touchdisp-dispatch-probe/artifacts/verdict.json` | OK |
| `touchdisp-gesture.yaml` | pass | pass |  | 3 | `04-scenarios/touchdisp-gesture/artifacts/verdict.json` | OK |
| `touchdisp-posted-side-effect.yaml` | pass | pass |  | 5 | `04-scenarios/touchdisp-posted-side-effect/artifacts/verdict.json` | OK |
| `v1-counter.yaml` | pass | pass |  | 13 | `04-scenarios/v1-counter/artifacts/verdict.json` | OK |
| `v1-hello-views.yaml` | pass | pass |  | 13 | `04-scenarios/v1-hello-views/artifacts/verdict.json` | OK |
| `v1-offscreen-pager.yaml` | pass | pass |  | 9 | `04-scenarios/v1-offscreen-pager/artifacts/verdict.json` | OK |
| `v1-recreate.yaml` | pass | pass |  | 18 | `04-scenarios/v1-recreate/artifacts/verdict.json` | OK |
| `v1-reset.yaml` | pass | pass |  | 11 | `04-scenarios/v1-reset/artifacts/verdict.json` | OK |
| `v2-art-counter.yaml` | pass | pass |  | 11 | `04-scenarios/v2-art-counter/artifacts/verdict.json` | OK |
| `v2-art-type-unsupported.yaml` | pass | pass |  | 3 | `04-scenarios/v2-art-type-unsupported/artifacts/verdict.json` | OK |
| `w15-broken-crash.yaml` | pass | error | GUEST_EXITED |  | `` | **NOT OK** |
| `w15-broken-missing-symbol.yaml` | pass | error | GUEST_EXITED |  | `` | **NOT OK** |
| `w15-canvas-shapes.yaml` | pass | pass |  | 2 | `04-scenarios/w15-canvas-shapes/artifacts/verdict.json` | OK |
| `w15-dialog-menu.yaml` | pass | pass |  | 3 | `04-scenarios/w15-dialog-menu/artifacts/verdict.json` | OK |
| `w15-dialogs-menus.yaml` | pass | pass |  | 3 | `04-scenarios/w15-dialogs-menus/artifacts/verdict.json` | OK |
| `w15-fragments-nav.yaml` | pass | pass |  | 3 | `04-scenarios/w15-fragments-nav/artifacts/verdict.json` | OK |
| `w15-list-adapter.yaml` | pass | pass |  | 3 | `04-scenarios/w15-list-adapter/artifacts/verdict.json` | OK |
| `w15-list-scroll.yaml` | pass | pass |  | 2 | `04-scenarios/w15-list-scroll/artifacts/verdict.json` | OK |
| `w15-native-hello.yaml` | pass | pass |  | 2 | `04-scenarios/w15-native-hello/artifacts/verdict.json` | OK |
| `w15-native-hostile.yaml` | pass | pass |  | 2 | `04-scenarios/w15-native-hostile/artifacts/verdict.json` | OK |
| `w15-rn-hello.yaml` | pass | pass |  | 2 | `04-scenarios/w15-rn-hello/artifacts/verdict.json` | OK |
| `w15-service-provider.yaml` | pass | pass |  | 3 | `04-scenarios/w15-service-provider/artifacts/verdict.json` | OK |
| `w15-sqlite-crud.yaml` | pass | pass |  | 2 | `04-scenarios/w15-sqlite-crud/artifacts/verdict.json` | OK |
| `w15-text-input.yaml` | pass | pass |  | 5 | `04-scenarios/w15-text-input/artifacts/verdict.json` | OK |
| `w15-todo-dogfood.yaml` | pass | pass |  | 4 | `04-scenarios/w15-todo-dogfood/artifacts/verdict.json` | OK |
| `w15-two-activity.yaml` | pass | pass |  | 3 | `04-scenarios/w15-two-activity/artifacts/verdict.json` | OK |
| `w15-widget-gallery.yaml` | pass | pass |  | 3 | `04-scenarios/w15-widget-gallery/artifacts/verdict.json` | OK |
| `w16-text-input.yaml` | pass | pass |  | 7 | `04-scenarios/w16-text-input/artifacts/verdict.json` | OK |
| `w17-form-hint.yaml` | pass | pass |  | 4 | `04-scenarios/w17-form-hint/artifacts/verdict.json` | OK |
| `w17-hello-views-hint.yaml` | pass | pass |  | 5 | `04-scenarios/w17-hello-views-hint/artifacts/verdict.json` | OK |
| `w17-multi-hint.yaml` | pass | pass |  | 3 | `04-scenarios/w17-multi-hint/artifacts/verdict.json` | OK |
| `w17-todo-hint.yaml` | pass | pass |  | 4 | `04-scenarios/w17-todo-hint/artifacts/verdict.json` | OK |
| `w19-form-behavior.yaml` | pass | pass |  | 8 | `04-scenarios/w19-form-behavior/artifacts/verdict.json` | OK |
| `w19-probe-content-desc.yaml` | pass | pass |  | 3 | `04-scenarios/w19-probe-content-desc/artifacts/verdict.json` | OK |
| `w19-probe-widgets.yaml` | pass | pass |  | 9 | `04-scenarios/w19-probe-widgets/artifacts/verdict.json` | OK |
| `w19-rn-wait-timeout.yaml` | pass | pass |  | 2 | `04-scenarios/w19-rn-wait-timeout/artifacts/verdict.json` | OK |
| `w21-hello-jni.yaml` | pass | pass |  | 10 | `04-scenarios/w21-hello-jni/artifacts/verdict.json` | OK |
| `w21-native-hello.yaml` | pass | pass |  | 2 | `04-scenarios/w21-native-hello/artifacts/verdict.json` | OK |
| `w21-rn-hello.yaml` | pass | pass |  | 5 | `04-scenarios/w21-rn-hello/artifacts/verdict.json` | OK |
| `w22-counter-log.yaml` | pass | pass |  | 6 | `04-scenarios/w22-counter-log/artifacts/verdict.json` | OK |
| `w22-counter-reset.yaml` | pass | pass |  | 9 | `04-scenarios/w22-counter-reset/artifacts/verdict.json` | OK |
| `w22-key-delete.yaml` | pass | pass |  | 9 | `04-scenarios/w22-key-delete/artifacts/verdict.json` | OK |
| `w22-type-clear.yaml` | pass | pass |  | 7 | `04-scenarios/w22-type-clear/artifacts/verdict.json` | OK |
| `w23-rn-hello-tap.yaml` | pass | pass |  | 6 | `04-scenarios/w23-rn-hello-tap/artifacts/verdict.json` | OK |
| `w24-counter-labels.yaml` | pass | pass |  | 7 | `04-scenarios/w24-counter-labels/artifacts/verdict.json` | OK |
| `w25-gaten-launch.yaml` | pass | error | GUEST_EXITED |  | `` | **NOT OK** |
| `w25-gaten-tap.yaml` | pass | pass |  | 4 | `04-scenarios/w25-gaten-tap/artifacts/verdict.json` | OK |
| `w26-gaten-debug-launch.yaml` | pass | error | GUEST_EXITED |  | `` | **NOT OK** |
| `w27-dialog-window.yaml` | pass | pass |  | 9 | `04-scenarios/w27-dialog-window/artifacts/verdict.json` | OK |
| `w28-gaten-debug-launch.yaml` | pass | error | GUEST_EXITED |  | `` | **NOT OK** |
| `w28-gaten-release-tap.yaml` | pass | pass |  | 4 | `04-scenarios/w28-gaten-release-tap/artifacts/verdict.json` | OK |
| `w31-gaten-release-evidence.yaml` | pass | pass |  | 5 | `04-scenarios/w31-gaten-release-evidence/artifacts/verdict.json` | OK |
| `w38-multi-activity.yaml` | pass | pass |  | 14 | `04-scenarios/w38-multi-activity/artifacts/verdict.json` | OK |
| `w42-back-frame.yaml` | pass | pass |  | 17 | `04-scenarios/w42-back-frame/artifacts/verdict.json` | OK |
| `w44-hostile-forest-untrusted.yaml` | pass | fail | INSUFFICIENT_HOST_EVIDENCE | 2 | `04-scenarios/w44-hostile-forest-untrusted/artifacts/verdict.json` | **NOT OK** |
| `w44-hostile-parcel.yaml` | pass | timeout | TIMEOUT | 2 | `04-scenarios/w44-hostile-parcel/artifacts/verdict.json` | **NOT OK** |
| `w44-hostile-probe-spam-trusted.yaml` | pass | pass |  | 2 | `04-scenarios/w44-hostile-probe-spam-trusted/artifacts/verdict.json` | OK |
| `w44-hostile-probe-spam.yaml` | pass | fail | INSUFFICIENT_HOST_EVIDENCE | 2 | `04-scenarios/w44-hostile-probe-spam/artifacts/verdict.json` | **NOT OK** |
| `w54-dialog-pixels.yaml` | pass | pass |  | 14 | `04-scenarios/w54-dialog-pixels/artifacts/verdict.json` | OK |
| `w7-insets-carve-negative.yaml` | fail | fail | ASSERTION_FAILED | 4 | `04-scenarios/w7-insets-carve-negative/artifacts/verdict.json` | OK |
| `w7-insets-report.yaml` | pass | pass |  | 10 | `04-scenarios/w7-insets-report/artifacts/verdict.json` | OK |

163 rows, 146 as declared, 17 not as declared
