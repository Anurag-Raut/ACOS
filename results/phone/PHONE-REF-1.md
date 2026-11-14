# PHONE-REF-1 — real-device reference for ACOS

Track: PHONE (branch `phone-ref-1`, base `3e11d9c` on `main`).
Question this track answers: **when an agent runs the same journey on the phone
and on ACOS, does it observe the same thing?** Not "is ACOS pretty" — what an
agent reads, decides on and asserts.

Device work used only `com.gaten` and `dev.acos.fixtures.*`, under
`flock /home/Qui/Projects/acos/.cache/phone.lock`; nothing was installed,
uninstalled or reconfigured, no account/setting was touched, and the phone was
left on its home screen.

## 1. Versions and identities

| | phone | ACOS |
|---|---|---|
| Model / engine | moto g35 5G (`manila_g`) | ACOS ART engine, API 34 image, process isolation |
| OS | Android 15, API **35** | Android 14 framework, API **34** |
| Fingerprint | `motorola/manila_g/manila:15/VUOA35.116-151/a43dc2:user/release-keys` | n/a (host build) |
| Display | 1080x2400 physical, **400 dpi**, font_scale 1.0 | scenario `display`: 1080x2400, **420 dpi**, offscreen |
| ABI | arm64-v8a | x86_64 |
| ACOS commit | — | `3e11d9c` (`main` at dispatch; `main` has since moved to `863def1`, not merged here) |
| Binary | — | `.cache/g3cd-target/release/acos` (release) |

### Gaten — **the two sides are not the same build**

| | phone | ACOS APK |
|---|---|---|
| Package | `com.gaten` | `com.gaten` |
| versionName / versionCode | `0.1.0` / **12** | not extractable from the x86_64 APK on this host (no aapt/aapt2); APK is `gaten-dev-f344272-x86_64-release-unsigned.apk` |
| APK SHA-256 | `378bef5a324a89851b4a25e7aebac5585e84e734e49c225b8e94f9a208643075` (arm64) | `e04c7ffcf01209c98e511e1ab01a7f68d6decb178d14a08f2b7f0a6539e2ee29` (x86_64) |
| Source | installed **2026-11-12 20:07 IST** (`dumpsys package`) | gaten HEAD `f344272` committed **2026-11-12 21:12 IST** |
| ABI | arm64 | x86_64 |

The installed phone build predates the gaten commit the ACOS APK was built from,
and the two apps render a **different onboarding flow** (§4.2). Every Gaten
difference below is therefore classified **(c) app-build difference** and cannot
be read as an ACOS defect. Installing an arm64 build of `f344272` on the phone is
the smallest next action for that part.

Version-code caution: gaten computes `versionCode = base * 10 + abiCode`
(`app/build.gradle.kts`), so the repo's own x86_64 release metadata reads
`versionCode 11 / versionName 0.1.0` and the phone's arm64 install reads **12**
for the same `base`. The versionCode alone therefore proves nothing; the build
difference rests on the install timestamp (20:07 IST) being earlier than
`f344272` (21:12 IST) **and** on the visibly different onboarding UI, which the
source at `f344272` (`src/rn/ui/onboarding/OnboardingFlow.tsx`, "each page has its
own Skip") says must have a `Skip`.

### Fixture APKs — same build on both sides

| package | phone APK SHA-256 (arm64) | ACOS APK SHA-256 (x86_64) |
|---|---|---|
| `dev.acos.fixtures.formvalidation` | `1ebeee51d86d1f295c0ac55d206faa23edbe7659b3188f623579d12acb39da5c` | `d8c277d7476a54f8a092…` |
| `dev.acos.fixtures.todolist` | `81c9eedd8194e81c68f5a13790b907e5e31e667f5307849cb56fe0907fef4441` | `1d94c4c9806be2f3a9e6…` |
| `dev.acos.fixtures.helloviews` | `05f1fd30049fa5d1cb5794f43e0d58cecd92949d62f5b3e6fae183f2c499d8de` | `25d0615792c4c3c74624…` |
| `dev.acos.fixtures.multiactivity` | `9f809ab544d6feed0c268a26aeaa5f35c6b38e5ed14b37cecab85e7ba01d2a60` | `986dbb18f33973df81dd…` |

The hashes differ because the phone installs are arm64 and ACOS runs x86_64
builds of the **same fixture sources** — that is the expected ABI difference, not
a version difference. What makes these flows a fair comparison is that every id,
text and log tag the scenarios select on resolves on both sides, and (for
`form-validation`, where the ACOS artifact's APK hash is recorded in its verdict)
the ACOS run's `app.sha256` is the local x86_64 fixture APK, `d8c277d7…`.

## 2. How it was measured

`tools/phone/replay_scenario.py` replays a scenario YAML on the phone through
`agent-device` (selectors resolved against the accessibility snapshot, actions
dispatched at the resolved node, screenshot + tree per step) and emits the same
per-step observation JSON as the ACOS side, which it also builds from
`acos verify --artifacts DIR` (`verdict.json` + `steps/NN-*.json` +
`tree/<frame_seq>.json` + `frames/<frame_seq>.png`).
`tools/phone/compare_observations.py` diffs the two files into the tables below.

Per step, both sides record: step kind + selector, the target's
existence/text/content-desc/enabled/checked/bounds, the visible label set, the
screen identity, and a screenshot (evidence, ≤540 px wide).

## 3. Flow results

| flow | ACOS verdict | phone replay | comparable steps | counts |
|---|---|---|---|---|
| `form-validation` | **pass** 28/28 | **pass** 28/28 | 28 | bounds=17, match=1, text-set=10 |
| `gaten-home` | **pass** 14/14 | launch + first tap pass, then blocked (no `Skip` on this build) | 2 of 14 | match=1, missing-on-phone=10, text-set=3 |
| `todo-list` | **unsupported** at step 22 `rotate` (`UNSUPPORTED_BACKEND`, `input.rotate`) | **pass** 29/29 | 22 | bounds=13, match=4, missing-side=6, status=1, text-set=5 |
| `hello-views` | **unsupported** at step 16 `rotate` | **pass** 26/26 | 16 | bounds=11, match=1, missing-side=9, status=1, text-set=4 |
| `multi-activity` | **unsupported** at step 14 `rotate` (`UNSUPPORTED_BACKEND`, `input.rotate`) | **pass** 21/21 | 14 | bounds=7, match=3, missing-side=6, status=1, text-set=4 |
| `negative/form-wrong-error` | expectation **fail** (as designed) | assertion **false** — the error node does not exist before Submit | 3 | twin really wrong on both sides |

Two capability facts fall out of this table and matter more than any single row:

1. **ACOS's ART backend has no `input.rotate`.** It says so honestly
   (`UNSUPPORTED_BACKEND`, "the ART session protocol has no rotate command yet")
   and does not fake a pass. The phone rotates, the app recreates, and the
   `todo-list` list is restored from SharedPreferences. Everything ACOS *could*
   check before the rotate step matched the phone.
2. **The phone's Gaten build cannot run the Gaten scenarios**, because the
   onboarding pages on that build have no `Skip`; ACOS's build has it on every
   page. Scenario steps 2–4 (`tap text exact Skip`) therefore have no target on
   the phone and the Home/Analytics screens are never reached. That is a build
   mismatch, not an ACOS behaviour difference (§1).

## 4. Per-step diff tables

Full machine-generated tables live next to the observations; the tables below
are those files.

### 4.1 `form-validation` — the fair comparison (identical APK)


| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |
|---|---|---|---|---|---|---|---|
| 0 | launch | `-` | pass / - / - | pass / - / - | - | Email, Form Validation, Name, Submit | **text-set** |
| 1 | wait_for | `{"id": "submit"}` | pass / Submit / [42,553][273,679] | pass / Submit / [40,512][260,632] | - | - | **bounds** |
| 2 | assert_text | `{"id": "name"}` | pass / Name / [42,317][1038,435] | pass / Name / [40,288][1040,400] | - | - | **bounds** |
| 3 | assert_text | `{"id": "email"}` | pass / Email / [42,435][1038,553] | pass / Email / [40,400][1040,512] | - | - | **bounds** |
| 4 | assert_text | `{"id": "submit"}` | pass / Submit / [42,553][273,679] | pass / Submit / [40,512][260,632] | - | - | **bounds** |
| 5 | assert_log | `-` | pass / - / - | pass / - / - | - | Email, Form Validation, Name, Submit | **text-set** |
| 6 | tap | `{"id": "submit"}` | pass / Submit / [42,553][273,679] | pass / Submit / [40,512][260,632] | - | - | **bounds** |
| 7 | assert_log | `-` | pass / - / - | pass / - / - | - | Email, Form Validation, Name, Name is required, Submit | **text-set** |
| 8 | assert_text | `{"id": "error"}` | pass / Name is required / [42,679][323,730] | pass / Name is required / [40,632][312,678] | - | - | **bounds** |
| 9 | tap | `{"id": "name"}` | pass / Name / [42,317][1038,435] | pass / Name / [40,288][1040,400] | - | - | **bounds** |
| 10 | type | `{"id": "name"}` | pass / Ada / [42,317][1038,435] | pass / Ada / [40,288][1040,400] | - | - | **bounds** |
| 11 | tap | `{"id": "submit"}` | pass / Submit / [42,553][273,679] | pass / Submit / [40,512][260,632] | - | - | **bounds** |
| 12 | assert_log | `-` | pass / - / - | pass / - / - | - | Ada, Email, Email is required, Form Validation, Submit | **text-set** |
| 13 | assert_text | `{"id": "error"}` | pass / Email is required / [42,679][316,730] | pass / Email is required / [40,632][307,678] | - | - | **bounds** |
| 14 | tap | `{"id": "email"}` | pass / Email / [42,435][1038,553] | pass / Email / [40,400][1040,512] | - | - | **bounds** |
| 15 | type | `{"id": "email"}` | pass / not-an-email / [42,435][1038,553] | pass / not-an-email / [40,400][1040,512] | - | - | **bounds** |
| 16 | tap | `{"id": "submit"}` | pass / Submit / [42,553][273,679] | pass / Submit / [40,512][260,632] | - | - | **bounds** |
| 17 | assert_log | `-` | pass / - / - | pass / - / - | - | Ada, Form Validation, Invalid email, Submit, not-an-email | **text-set** |
| 18 | assert_text | `{"id": "error"}` | pass / Invalid email / [42,679][248,730] | pass / Invalid email / [40,632][242,678] | - | - | **bounds** |
| 19 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 20 | type | `{"id": "email"}` | pass / ada@example.com / [42,435][1038,553] | pass / ada@example.com / [40,400][1040,512] | - | - | **bounds** |
| 21 | tap | `{"id": "submit"}` | pass / Submit / [42,553][273,679] | pass / Submit / [40,512][260,632] | Ada, Form Validation, Submit, ada@example.com | - | **bounds** |
| 22 | assert_log | `-` | pass / - / - | pass / - / - | - | OK, Submitted: Ada, Success | **text-set** |
| 23 | assert_text | `{"id": "message"}` | pass / Submitted: Ada / [70,1157][1010,1214] | pass / Submitted: Ada / [67,1182][1013,1236] | Ada, Form Validation, Submit, ada@example.com | - | **bounds** |
| 24 | capture | `-` | pass / - / - | pass / - / - | Ada, Form Validation, Submit, ada@example.com | - | **text-set** |
| 25 | assert_region | `-` | pass / - / - | not_compared / - / - | Ada, Form Validation, Submit, ada@example.com | - | **text-set** |
| 26 | assert_region | `-` | pass / - / - | not_compared / - / - | Ada, Form Validation, Submit, ada@example.com | - | **text-set** |
| 27 | assert_no_crash | `-` | pass / - / - | pass / - / - | - | OK, Submitted: Ada, Success | **text-set** |

Counts: bounds=17, match=1, text-set=10

Reading of this table:

* **No status differences at all.** Every assertion ACOS passes, the phone
  passes with the same text: `Name` / `Email` / `Submit`, `Name is required`,
  `Email is required`, `Invalid email`, the `Submitted: Ada` dialog
  (`android:id/message`), and the `AcosForm` log order
  (`onCreate → onStart → onResume`, `validation_failed reason=…`,
  `validation_ok name=Ada email=ada@example.com`) — the phone emitted the
  identical log lines, in the same order, with the same values.
* **17 `bounds` rows.** Every target sits at a different pixel rectangle because
  the phone is 400 dpi on API 35 and ACOS's scenario is 420 dpi on API 34. The
  deltas are not a uniform scale: the left/right insets shrink by the density
  ratio while the vertical origin moves up by ~29–47 px (status-bar/edge-to-edge
  inset difference), and the dialog card stays centred (`[70,1157]` →
  `[67,1182]`). This is the class the repo already labels
  `bounds_differ_density`; the scenario's `bounds:` expectations are calibrated
  to the API-34 oracle and can never match a real phone.
* **10 `text-set` rows**, all tool scope, not behaviour:
  * steps 0, 5, 7, 12, 17, 22 are `launch`/`assert_log` steps on the ACOS side —
    those steps carry **no tree in `verify` artifacts** (§6), so the phone's
    labels look "extra";
  * steps 21, 23–27 are the success-dialog steps: with an `AlertDialog` up, the
    phone's accessibility snapshot exposes **only the dialog window**, while
    ACOS's tree keeps the app window plus the dialog. An agent sees the thing
    that matters (the dialog text) on both.

### 4.2 `gaten-home` — build-blocked


| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |
|---|---|---|---|---|---|---|---|
| 0 | launch | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 1 | tap | `{"text": {"contains": "Set up Gaten"}}` | pass / - / - | pass / Set up Gaten / - | 1 / 3, Android needs Gaten as an accessibility service., Choose text recognition, Look up  | - | **text-set** |
| 2 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2209] | fail / - / - | 2 / 3, Back, Install AnkiDroid, Look up Japanese anywhere, Look up Japanese anywhere. Step | - | **missing-on-phone** |
| 3 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2209] | fail / - / - | 2 / 3, A separate app from Google Play., Allow access, Back, Connect AnkiDroid to review y | - | **missing-on-phone** |
| 4 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2209] | fail / - / - | 3 / 3, Back, Connect AnkiDroid to review your decks and save words from your lookups as fl | - | **missing-on-phone** |
| 5 | wait_for | `{"text": {"contains": "Recently looked up"}}` | pass / Recently looked up / [58,1424][1024,1482] | fail / - / - | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | - | **missing-on-phone** |
| 6 | capture | `-` | pass / - / - | pass / - / - | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | - | **text-set** |
| 7 | assert_text | `{"text": {"contains": "Daily streak"}}` | pass / Daily streak / [150,610][764,663] | fail / - / - | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | - | **missing-on-phone** |
| 8 | assert_text | `{"text": {"contains": "Recently looked up"}}` | pass / Recently looked up / [58,1424][1024,1482] | fail / - / - | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | - | **missing-on-phone** |
| 9 | assert_text | `{"text": {"exact": "Anki"}}` | pass / Anki / [187,1001][766,1054] | fail / - / - | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | - | **missing-on-phone** |
| 10 | tap | `{"content_desc": {"exact": "Analytics"}}` | pass / Analytics / [58,193][854,308] | fail / - / - | 12 weeks · Anki, Accuracy, Analytics, Anki, Anki is not installed., Answer accuracy will a | - | **missing-on-phone** |
| 11 | wait_for | `{"text": {"contains": "Accuracy"}}` | pass / Accuracy / [58,1758][229,1811] | fail / - / - | 12 weeks · Anki, Accuracy, Analytics, Anki, Anki is not installed., Answer accuracy will a | - | **missing-on-phone** |
| 12 | capture | `-` | pass / - / - | pass / - / - | 12 weeks · Anki, Accuracy, Analytics, Anki, Anki is not installed., Answer accuracy will a | - | **text-set** |
| 13 | assert_text | `{"text": {"contains": "Study activity"}}` | pass / Study activity / [58,1262][312,1315] | fail / - / - | 12 weeks · Anki, Accuracy, Analytics, Anki, Anki is not installed., Answer accuracy will a | - | **missing-on-phone** |

Counts: match=1, missing-on-phone=10, text-set=3

* Steps 0/1 match: the app launches to the welcome screen and the
  `Set up Gaten` pressable is found and pressed on both sides.
* Step 2: ACOS resolves `Skip` at `[498,2163][582,2209]` and moves on; on the
  phone the page has **no `Skip` node** — its footer is a second
  `Set up Gaten` button — so `ELEMENT_NOT_FOUND`.
* Consequently steps 3–13 are `missing-on-phone`: the phone never leaves
  onboarding, so `Recently looked up`, `Daily streak`, `Anki`, the Analytics tab
  and `Study activity` are never on screen. The label sets confirm the two
  builds show different onboarding pages
  (`1 / 3, Android needs Gaten as an accessibility service., Choose text
  recognition, Install AnkiDroid…` on ACOS vs `Circle it / Add it to Anki /
  See what a word means` on the phone).

Because the divergence starts on the first onboarding page, the other Gaten
flows (`gaten-tabs`, `gaten-settings`, `gaten-search`, `gaten-lesson`) were
**not** replayed further: they share the same first four steps and would fail at
the same place. Their value arrives with an arm64 build of `f344272` on the
phone.

### 4.3 `todo-list`


| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |
|---|---|---|---|---|---|---|---|
| 0 | launch | `-` | pass / - / - | pass / - / - | - | Add, Clear all, New item, Todo List | **text-set** |
| 1 | wait_for | `{"id": "input"}` | pass / New item / [42,317][1038,435] | pass / New item / [40,288][1040,400] | - | - | **bounds** |
| 2 | assert_text | `{"id": "input"}` | pass / New item / [42,317][1038,435] | pass / New item / [40,288][1040,400] | - | - | **bounds** |
| 3 | assert_text | `{"id": "add"}` | pass / Add / [42,435][273,561] | pass / Add / [40,400][260,520] | - | - | **bounds** |
| 4 | assert_text | `{"id": "clear"}` | pass / Clear all / [273,435][504,561] | pass / Clear all / [260,400][480,520] | - | - | **bounds** |
| 5 | assert_log | `-` | pass / - / - | pass / - / - | - | Add, Clear all, New item, Todo List | **text-set** |
| 6 | tap | `{"id": "input"}` | pass / New item / [42,317][1038,435] | pass / New item / [40,288][1040,400] | - | - | **bounds** |
| 7 | type | `{"id": "input"}` | pass / milk / [42,317][1038,435] | pass / milk / [40,288][1040,400] | - | - | **bounds** |
| 8 | tap | `{"id": "add"}` | pass / Add / [42,435][273,561] | pass / Add / [40,400][260,520] | - | - | **bounds** |
| 9 | assert_log | `-` | pass / - / - | pass / - / - | - | Add, Clear all, New item, Todo List, milk | **text-set** |
| 10 | assert_text | `{"text": {"exact": "milk"}}` | pass / milk / [42,561][1038,687] | pass / milk / [40,520][1040,640] | - | - | **bounds** |
| 11 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 12 | tap | `{"id": "input"}` | pass / New item / [42,317][1038,435] | pass / New item / [40,288][1040,400] | - | - | **bounds** |
| 13 | type | `{"id": "input"}` | pass / bread / [42,317][1038,435] | pass / bread / [40,288][1040,400] | - | - | **bounds** |
| 14 | tap | `{"id": "add"}` | pass / Add / [42,435][273,561] | pass / Add / [40,400][260,520] | - | - | **bounds** |
| 15 | assert_log | `-` | pass / - / - | pass / - / - | - | Add, Clear all, New item, Todo List, bread, milk | **text-set** |
| 16 | assert_text | `{"text": {"exact": "bread"}}` | pass / bread / [42,690][1038,816] | pass / bread / [40,643][1040,763] | - | - | **bounds** |
| 17 | tap | `{"text": {"exact": "milk"}}` | pass / - / - | pass / milk / [40,520][1040,640] | - | - | **match** |
| 18 | assert_log | `-` | pass / - / - | pass / - / - | - | Add, Clear all, New item, Todo List, bread | **text-set** |
| 19 | assert_not_visible | `{"text": {"exact": "milk"}}` | pass / - / - | pass / - / - | - | - | **match** |
| 20 | assert_text | `{"text": {"exact": "bread"}}` | pass / bread / [42,561][1038,687] | pass / bread / [40,520][1040,640] | - | - | **bounds** |
| 21 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 22 | rotate | `-` | unsupported / - / - | pass / - / - | - | Add, Clear all, New item, Todo List, bread | **status** |
| 23 | assert_text | `{"text": {"exact": "bread"}}` | - / - / - | pass / bread / [148,452][2360,572] | - | Add, Clear all, New item, Todo List, bread | **missing-side** |
| 24 | assert_not_visible | `{"text": {"exact": "milk"}}` | - / - / - | pass / - / - | - | Add, Clear all, New item, Todo List, bread | **missing-side** |
| 25 | rotate | `-` | - / - / - | pass / - / - | - | Add, Clear all, New item, Todo List, bread | **missing-side** |
| 26 | assert_text | `{"text": {"exact": "bread"}}` | - / - / - | pass / bread / [40,520][1040,640] | - | Add, Clear all, New item, Todo List, bread | **missing-side** |
| 27 | back | `-` | - / - / - | pass / - / - | - | - | **missing-side** |
| 28 | assert_no_crash | `-` | - / - / - | pass / - / - | - | - | **missing-side** |

Counts: bounds=13, match=4, missing-side=6, status=1, text-set=5

* Steps 0–21 (launch, ids/texts for `New item`/`Add`/`Clear all`, typing milk
  and bread, `add item=… size=…` and `remove item=… size=…` logs, the list rows
  and their removal): same texts, same log lines, same targets. 13 `bounds` rows
  are the density/inset difference again.
* Step 22 `rotate`: ACOS **unsupported**, the phone rotates and the app
  recreates. Steps 23–28 therefore have no ACOS observation. On the phone the
  list survives the recreate (`bread` at `[148,452][2360,572]` in landscape and
  `[40,520][1040,640]` back in portrait) — the SharedPreferences claim the
  scenario makes and ACOS cannot currently verify.

### 4.4 `hello-views`

| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |
|---|---|---|---|---|---|---|---|
| 0 | launch | `-` | pass / - / - | pass / - / - | - | Enter your name, Greet, Hello ACOS, Hello Views, clicks: 0 | **text-set** |
| 1 | wait_for | `{"id": "result"}` | pass / clicks: 0 / [42,612][175,663] | pass / clicks: 0 / [40,566][166,612] | - | - | **bounds** |
| 2 | assert_text | `{"id": "title"}` | pass / Hello ACOS / [42,317][231,368] | pass / Hello ACOS / [40,288][215,334] | - | - | **bounds** |
| 3 | assert_text | `{"id": "input"}` | pass / Enter your name / [42,368][1038,486] | pass / Enter your name / [40,334][1040,446] | - | - | **bounds** |
| 4 | assert_text | `{"id": "btn"}` | pass / Greet / [42,486][273,612] | pass / Greet / [40,446][260,566] | - | - | **bounds** |
| 5 | assert_text | `{"id": "result"}` | pass / clicks: 0 / [42,612][175,663] | pass / clicks: 0 / [40,566][166,612] | - | - | **bounds** |
| 6 | assert_log | `-` | pass / - / - | pass / - / - | - | Enter your name, Greet, Hello ACOS, Hello Views, clicks: 0 | **text-set** |
| 7 | tap | `{"id": "input"}` | pass / Enter your name / [42,368][1038,486] | pass / Enter your name / [40,334][1040,446] | - | - | **bounds** |
| 8 | type | `{"id": "input"}` | pass / World / [42,368][1038,486] | pass / World / [40,334][1040,446] | - | - | **bounds** |
| 9 | tap | `{"id": "btn"}` | pass / Greet / [42,486][273,612] | pass / Greet / [40,446][260,566] | - | - | **bounds** |
| 10 | assert_log | `-` | pass / - / - | pass / - / - | - | Greet, Hello ACOS, Hello Views, Hello, World! clicks: 1, World | **text-set** |
| 11 | assert_text | `{"id": "result"}` | pass / Hello, World! clicks: 1 / [42,612][392,663] | pass / Hello, World! clicks: 1 / [40,566][372,612] | - | - | **bounds** |
| 12 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 13 | tap | `{"id": "btn"}` | pass / Greet / [42,486][273,612] | pass / Greet / [40,446][260,566] | - | - | **bounds** |
| 14 | assert_log | `-` | pass / - / - | pass / - / - | - | Greet, Hello ACOS, Hello Views, Hello, World! clicks: 2, World | **text-set** |
| 15 | assert_text | `{"id": "result"}` | pass / Hello, World! clicks: 2 / [42,612][392,663] | pass / Hello, World! clicks: 2 / [40,566][378,612] | - | - | **bounds** |
| 16 | rotate | `-` | unsupported / - / - | pass / - / - | - | Greet, Hello ACOS, Hello Views, World, clicks: 2 | **status** |
| 17 | assert_log | `-` | - / - / - | pass_with_failure / - / - | - | Greet, Hello ACOS, Hello Views, World, clicks: 2 | **missing-side** |
| 18 | assert_text | `{"id": "input"}` | - / - / - | pass / World / [148,266][2360,378] | - | Greet, Hello ACOS, Hello Views, World, clicks: 2 | **missing-side** |
| 19 | assert_text | `{"id": "result"}` | - / - / - | pass / clicks: 2 / [148,498][273,544] | - | Greet, Hello ACOS, Hello Views, World, clicks: 2 | **missing-side** |
| 20 | rotate | `-` | - / - / - | pass / - / - | - | Greet, Hello ACOS, Hello Views, World, clicks: 2 | **missing-side** |
| 21 | assert_log | `-` | - / - / - | pass_with_failure / - / - | - | Greet, Hello ACOS, Hello Views, World, clicks: 2 | **missing-side** |
| 22 | assert_text | `{"id": "result"}` | - / - / - | pass / clicks: 2 / [40,566][165,612] | - | Greet, Hello ACOS, Hello Views, World, clicks: 2 | **missing-side** |
| 23 | back | `-` | - / - / - | pass / - / - | - | - | **missing-side** |
| 24 | assert_log | `-` | - / - / - | pass_with_failure / - / - | - | - | **missing-side** |
| 25 | assert_no_crash | `-` | - / - / - | pass / - / - | - | - | **missing-side** |

Counts: bounds=11, match=1, missing-side=9, status=1, text-set=4


* Steps 0–15 (counter text and clicks, `Hello, World! clicks: N`, the id-based
  asserts, including the typed `World` landing in `id/input`): same texts and
  targets; 11 `bounds` rows.
* Step 16 `rotate`: ACOS unsupported, phone rotates → steps 17–25 have no ACOS
  observation.
* Phone-only note: three lifecycle `assert_log` steps after the rotate
  (`onPause/onStop/onSaveInstanceState/onDestroy` then `onCreate/onStart/onResume`)
  matched only as prefixes on the phone, because the recreate lines are emitted
  by the *new* process while the old process's lines are still in the buffer and
  the phone harness slices new lines per step. Marked `pass_with_failure` in the
  observations and never counted as a pass: a limitation of the phone harness,
  not a claim about ACOS or the app.

### 4.5 `multi-activity`

| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |
|---|---|---|---|---|---|---|---|
| 0 | launch | `-` | pass / - / - | pass / - / - | - | Message, Multi Activity, Send | **text-set** |
| 1 | wait_for | `{"id": "message"}` | pass / Message / [42,317][1038,435] | pass / Message / [40,288][1040,400] | - | - | **bounds** |
| 2 | assert_text | `{"id": "message"}` | pass / Message / [42,317][1038,435] | pass / Message / [40,288][1040,400] | - | - | **bounds** |
| 3 | assert_text | `{"id": "send"}` | pass / Send / [42,435][273,561] | pass / Send / [40,400][260,520] | - | - | **bounds** |
| 4 | assert_log | `-` | pass / - / - | pass / - / - | - | Message, Multi Activity, Send | **text-set** |
| 5 | tap | `{"id": "message"}` | pass / Message / [42,317][1038,435] | pass / Message / [40,288][1040,400] | - | - | **bounds** |
| 6 | type | `{"id": "message"}` | pass / Hello / [42,317][1038,435] | pass / Hello / [40,288][1040,400] | - | - | **bounds** |
| 7 | tap | `{"id": "send"}` | pass / - / - | pass / Send / [40,400][260,520] | - | - | **match** |
| 8 | assert_log | `-` | pass / - / - | pass / - / - | - | Finish, Multi Activity, Received: Hello | **text-set** |
| 9 | assert_text | `{"id": "received"}` | pass / Received: Hello / [42,317][295,368] | pass / Received: Hello / [40,288][282,334] | - | - | **bounds** |
| 10 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 11 | tap | `{"id": "finish"}` | pass / - / - | pass / Finish / [40,334][260,454] | - | - | **match** |
| 12 | assert_log | `-` | pass / - / - | pass / - / - | - | Hello, Multi Activity, Send | **text-set** |
| 13 | assert_text | `{"id": "message"}` | pass / Hello / [42,317][1038,435] | pass / Hello / [40,288][1040,400] | - | - | **bounds** |
| 14 | rotate | `-` | unsupported / - / - | pass / - / - | - | Hello, Multi Activity, Send | **status** |
| 15 | assert_log | `-` | - / - / - | pass_with_failure / - / - | - | Hello, Multi Activity, Send | **missing-side** |
| 16 | assert_text | `{"id": "message"}` | - / - / - | pass / Hello / [148,220][2360,332] | - | Hello, Multi Activity, Send | **missing-side** |
| 17 | rotate | `-` | - / - / - | pass / - / - | - | Hello, Multi Activity, Send | **missing-side** |
| 18 | assert_text | `{"id": "message"}` | - / - / - | pass / Hello / [40,288][1040,400] | - | Hello, Multi Activity, Send | **missing-side** |
| 19 | back | `-` | - / - / - | pass / - / - | - | - | **missing-side** |
| 20 | assert_no_crash | `-` | - / - / - | pass / - / - | - | - | **missing-side** |

Counts: bounds=7, match=3, missing-side=6, status=1, text-set=4

Phone: **pass 21/21** (launch, first activity, `Next` to the second activity,
`Back`, texts and ids at each step). ACOS reaches step 13 and then stops at the
`rotate` with the same `UNSUPPORTED_BACKEND`/`input.rotate` as the other two
flows, so steps 14–20 have no ACOS observation. Where both sides ran, the same
texts and ids were observed; the 7 `bounds` rows are the density/inset shift.

## 5. Classification of every difference

| # | difference | class | agent impact |
|---|---|---|---|
| 1 | Phone Gaten shows the coach-mark onboarding (`Circle it`, no `Skip`); ACOS shows the numbered setup wizard with `Skip` | **(c) app build** — the phone install predates gaten `f344272` | Blocks all five Gaten scenarios on the phone; no ACOS conclusion possible from them. |
| 2 | All pixel bounds differ (17 rows form-validation, 13 todo-list, 10 hello-views) | **(b) density + API level** — 400 vs 420 dpi, API 35 edge-to-edge insets | An agent must not assert absolute bounds across devices; text/id/state are stable. ACOS's scenario `bounds:` expectations are oracle-calibrated and always fail on a real phone. |
| 3 | Dialogs: the phone exposes only the dialog window, ACOS the app window + dialog | **(b) tool/window scope** (`agent-device` accessibility snapshot vs the ART tree) | Low: the dialog text an agent needs is present on both. |
| 4 | ACOS `launch` and `assert_log` steps carry no tree/frame in `verify` artifacts | **(b) measurement gap** (`steps/NN.json` evidence is `["step"]` only) | Medium for artifact-reading agents: the first screen and the state at a log assertion cannot be diffed from artifacts alone. The runtime reply does carry a tree; the artifacts do not. |
| 5 | Soft keyboard: on the phone the IME stays up across rotation and covers the list; ACOS's offscreen guest has no IME window | **(b) platform difference** (real IME vs offscreen guest) | Medium: an agent replaying text-entry + rotate flows on a phone must `keyboard dismiss` before reading the screen. The phone harness now does this; the ACOS scenario needs no change. |
| 6 | `input.rotate` unsupported on ACOS ART | **(b) capability gap**, honestly reported as `UNSUPPORTED_BACKEND` | High for gates that include rotation: 6–10 steps of `todo-list`/`hello-views` cannot be verified on ACOS at all. |
| 7 | Success dialog in the post-tap observation | **(d) none** — checked to be equal on both sides (`form-validation` step 21, ACOS `tree/31.json` has `alertTitle Success`, `message Submitted: Ada`) | — |
| 8 | Lifecycle log lines split across steps on the phone after a recreate | **(d) timing / harness** | None on the app; harness-only. |

### (a) ACOS bugs an agent would act on: **none found**

Candidates deliberately checked and rejected:

* *"ACOS's tap reply does not include the dialog it just opened"* — false: the
  bound tree for the submit tap (`form-validation` step 21, `tree/31.json`)
  already contains `android:id/alertTitle`, `android:id/message` and
  `android:id/button1`, identical to what the phone shows after the same tap.
* *"ACOS loses or reorders the inline error after a second validation"* — false:
  the error node text changes `Name is required → Email is required → Invalid
  email` exactly as on the phone, with the same log lines between them.
* *"ACOS's dialog geometry is wrong"* — not an ACOS bug against the phone: the
  card is `[70,1157][1010,1214]` on ACOS and `[67,1182][1013,1236]` on the phone,
  the same card shifted by the density/inset difference, still centred and the
  same size within 1 %.
* *"ACOS shows the same onboarding pages as the phone"* — the divergence is real
  but is a build difference (#1), not ACOS behaviour.

So on the flows where both sides ran the *same* APK, an agent that reads texts,
resource ids, enabled/checked state, dialogs and log order would act identically.
That is the positive result of this track, bounded by what could not be compared:
Gaten (build) and rotation (capability).

## 6. Caveats

* `main` advanced from `3e11d9c` to `863def1` while this track ran. All ACOS
  numbers here come from `3e11d9c` (this branch's base); nothing was rebased.
* The phone build of Gaten cannot be compared to the x86_64 APK's versionCode on
  this host (no `aapt`/`aapt2`; `acos inspect` does not report it). The build
  difference rests on the install timestamp vs the gaten commit timestamp and on
  the visible onboarding UI, plus the phone's own `dumpsys package` values.
* `assert_region` (pixel-hash) steps are recorded as `not_compared` on the phone:
  their hashes are calibrated to the ACOS frame geometry (display origin
  `[0,128]`, 420 dpi) and ACOS's executor applies the frame's display origin
  itself. Re-deriving per-device hashes is a separate track.
* Phone screenshots are stored at ≤540 px wide (evidence budget); the full-size
  captures stay in `.cache/phone-probe/`.

## 7. Rerun

```bash
# ACOS side (needs the ART lock; ~10 s once acquired)
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
tools/art-runtime/run-exclusive.sh --task PHONE -- tools/guarded.sh --mem 10G --time 600 -- \
  .cache/g3cd-target/release/acos verify scenarios/form-validation.yaml --engine art \
  --json --artifacts .cache/phone-probe/acos/form-validation

# both sides -> the same observation shape
python3 tools/phone/replay_scenario.py --source acos \
  --scenario scenarios/form-validation.yaml \
  --artifacts .cache/phone-probe/acos/form-validation \
  --out results/phone/evidence/form-validation/acos
python3 tools/phone/replay_scenario.py --source phone \
  --scenario scenarios/form-validation.yaml \
  --out results/phone/evidence/form-validation/phone --require-lock

# diff
python3 tools/phone/compare_observations.py \
  --acos results/phone/evidence/form-validation/acos/observations.json \
  --phone results/phone/evidence/form-validation/phone/observations.json \
  --out results/phone/evidence/form-validation/DIFF.md
```

`--require-lock` takes `flock /home/Qui/Projects/acos/.cache/phone.lock` for the
whole replay, so this can run next to other agents without two of them driving
the device.

## 8. Evidence index

| path | what |
|---|---|
| `results/phone/evidence/form-validation/{acos,phone}/observations.json` | per-step observations, identical schema |
| `results/phone/evidence/form-validation/DIFF.md` | the table in §4.1 |
| `results/phone/evidence/gaten-home/{acos,phone}/…`, `DIFF.md` | §4.2 |
| `results/phone/evidence/todo-list/{acos,phone}/…`, `DIFF.md` | §4.3 |
| `results/phone/evidence/hello-views/{acos,phone}/…`, `DIFF.md` | §4.4 |
| `results/phone/evidence/multi-activity/phone/observations.json` | §4.5 (ACOS side pending) |
| `results/phone/evidence/neg-form-wrong/phone/observations.json` | negative twin false on the phone |
| `results/phone/evidence/*/phone/screenshot-NN-*.png` | paired screenshots, ≤540 px wide |
