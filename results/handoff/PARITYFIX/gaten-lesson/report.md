# Parity: gaten-lesson

* generated: 2026-11-18T08:54:56+00:00
* scenario: `scenarios/gaten-lesson.yaml` (sha256 `d45dd9f07f7d`)
* ACOS: `/home/Qui/.t3/worktrees/acos/t3-0a3f7c33-cca21cd9449033f1/.cache/pf/parity/gaten-lesson` (art / pass, 1080x2400 @400 dpi)
* phone: **recorded-observations** (physical-device (recorded capture)), coverage **text-set+target**, 1080x2400 @400 dpi, `results/phone/gaten-f344272/gaten-lesson/phone`
* bounds tolerance: 1.5 dp (bounds converted with each side's own density)

## Summary

* steps compared: **27/29** (2 without an ACOS tree)
* matched nodes: **74**, exact matches: **0**
* differences: **159** (0 documented platform differences, **159 unexpected**) in **86 findings**
* excluded as invalid evidence: 494 differences on steps [7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27] (phone replay failed earlier)

> **Phone replay failed at steps [7, 15, 20].** Past the first failure the phone is not on the screen the scenario asked for; those differences are classified `phone-scenario-failed` and say nothing about ACOS.

> **Phone observations that failed on a capture that cannot name attributes** (not evidence about ACOS; re-run with the phone attached): step 8 wait_for `{"text": {"exact": "Anki is not installed"}}` (TIMEOUT); step 10 assert_text `{"text": {"exact": "Anki is not installed"}}` (ASSERTION_FAILED); step 11 assert_text `{"text": {"exact": "Download Anki"}}` (ASSERTION_FAILED); step 13 wait_for `{"text": {"contains": "Recently looked up"}}` (TIMEOUT); step 16 wait_for `{"text": {"exact": "Connect WaniKani"}}` (TIMEOUT); step 18 assert_text `{"text": {"exact": "Connect WaniKani"}}` (ASSERTION_FAILED); step 19 assert_text `{"text": {"exact": "Personal access token"}}` (ASSERTION_FAILED); step 23 assert_text `{"content_desc": {"exact": "WaniKani personal access token"}}` (ASSERTION_FAILED); step 25 wait_for `{"text": {"contains": "Recently looked up"}}` (TIMEOUT); step 27 assert_text `{"text": {"contains": "Daily streak"}}` (ASSERTION_FAILED)

> The phone was not attached for this run: the comparison uses the checked-in physical-device capture below. It is real moto g35 data, but it is not a fresh replay; re-run with the phone attached to refresh it.
> This capture does not say it was read from `uiautomator dump` (a11y_source=None); none of its steps has a single `text` node, so every name was filed under content-desc by the agent-device mapping that results/handoff/TREE1.md found wrong. Whether a name is the text or the content-desc of its node is therefore not comparable here: it is reported as `attribute_placement_unverified` (needs a live phone replay), never as an ACOS finding and never as a pass.

## Per step

| # | step | ACOS frame | nodes A/P | matched | exact | allowed | unexpected |
|---|---|---|---|---|---|---|---|
| 0 | `launch com.gaten.app.MainActivity` | - | - | - | - | - | no_acos_tree |
| 1 | `tap {"text":{"contains":"Set up Gaten"}}` | 3 (own) | 175/61 | 11 | 0 | 0 | 4 |
| 2 | `tap {"text":{"exact":"Skip"}}` | 4 (own) | 162/66 | 4 | 0 | 0 | 16 |
| 3 | `tap {"text":{"exact":"Skip"}}` | 5 (own) | 162/225 | 2 | 0 | 0 | 56 |
| 4 | `tap {"text":{"exact":"Skip"}}` | 6 (own) | 162/165 | 0 | 0 | 0 | 49 |
| 5 | `wait_for {"text":{"contains":"Recently looked up"}}` | 7 (own) | 218/165 | 26 | 0 | 0 | 18 |
| 6 | `capture lesson-home` | 8 (own) | 218/165 | 26 | 0 | 0 | 16 |
| 7 | `tap {"content_desc":{"contains":"Anki, Not connected"}}` | 10 (own) | 233/165 | 1 | 0 | invalid | invalid |
| 8 | `wait_for {"text":{"exact":"Anki is not installed"}}` | 11 (own) | 233/165 | 1 | 0 | invalid | invalid |
| 9 | `capture lesson-anki-blocked` | 12 (own) | 233/165 | 1 | 0 | invalid | invalid |
| 10 | `assert_text {"text":{"exact":"Anki is not installed"}}` | 12 (own) | 233/165 | 1 | 0 | invalid | invalid |
| 11 | `assert_text {"text":{"exact":"Download Anki"}}` | 12 (own) | 233/165 | 1 | 0 | invalid | invalid |
| 12 | `back` | 13 (own) | 345/0 | 0 | 0 | invalid | invalid |
| 13 | `wait_for {"text":{"contains":"Recently looked up"}}` | 14 (own) | 345/0 | 0 | 0 | invalid | invalid |
| 14 | `capture lesson-home-again` | 15 (own) | 345/0 | 0 | 0 | invalid | invalid |
| 15 | `tap {"content_desc":{"contains":"WaniKani, Not connected"}}` | 17 (own) | 245/0 | 0 | 0 | invalid | invalid |
| 16 | `wait_for {"text":{"exact":"Connect WaniKani"}}` | 18 (own) | 245/0 | 0 | 0 | invalid | invalid |
| 17 | `capture lesson-wanikani-connect` | 19 (own) | 245/0 | 0 | 0 | invalid | invalid |
| 18 | `assert_text {"text":{"exact":"Connect WaniKani"}}` | 19 (own) | 245/0 | 0 | 0 | invalid | invalid |
| 19 | `assert_text {"text":{"exact":"Personal access token"}}` | 19 (own) | 245/0 | 0 | 0 | invalid | invalid |
| 20 | `tap {"content_desc":{"exact":"WaniKani personal access token"}}` | 21 (own) | 245/0 | 0 | 0 | invalid | invalid |
| 21 | `type not-a-real-token` | 22 (own) | 245/0 | 0 | 0 | invalid | invalid |
| 22 | `capture lesson-token-entered` | 23 (own) | 245/0 | 0 | 0 | invalid | invalid |
| 23 | `assert_text {"content_desc":{"exact":"WaniKani personal access token"}}` | 23 (own) | 245/0 | 0 | 0 | invalid | invalid |
| 24 | `back` | 24 (own) | 245/0 | 0 | 0 | invalid | invalid |
| 25 | `back` | 25 (own) | 345/0 | 0 | 0 | invalid | invalid |
| 26 | `wait_for {"text":{"contains":"Recently looked up"}}` | 26 (own) | 345/0 | 0 | 0 | invalid | invalid |
| 27 | `capture lesson-back-home` | 27 (own) | 345/0 | 0 | 0 | invalid | invalid |
| 28 | `assert_text {"text":{"contains":"Daily streak"}}` | - | - | - | - | - | no_phone_capture |

## Findings (unexpected differences)

| classification | kind | node | steps | # |
|---|---|---|---|---|
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `attr:accessible-name-placement` | 1, 2, 3, 5, 6 | 5 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Anki Fri: upcoming` | 5, 6 | 2 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Anki Sat: upcoming` | 5, 6 | 2 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Anki Thu: upcoming` | 5, 6 | 2 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Anki Wed: study activity unknown, today` | 5, 6 | 2 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Anki, Not connected, Open Anki` | 5, 6 | 2 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Daily streak. Anki study days follow your collection's rollover.` | 5, 6 | 2 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Look up Japanese anywhere. Step 1 of 3.` | 1, 2, 3 | 3 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Search Japanese dictionary` | 5, 6 | 2 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Step 1 of 3` | 1 | 1 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `text:Recently looked up` | 5 | 1 |
| labels/only-in-acos | label_only_in_acos | `label:Open Anki` | 5, 6 | 2 |
| labels/only-on-phone | label_only_on_phone | `label:Anki Fri: study activity unknown` | 3, 4, 5, 6 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Anki Sat: study activity unknown, today` | 3, 4, 5, 6 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Anki Thu: study activity unknown` | 3, 4, 5, 6 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Anki Wed: study activity unknown` | 3, 4, 5, 6 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Anki, Permission needed, Grant access` | 3, 4, 5, 6 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Grant access` | 3, 4, 5, 6 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Permission needed` | 3, 4, 5, 6 | 4 |
| layout/bounds | bounds_changed | `desc:Set up Gaten` | 1 | 1 |
| layout/bounds | bounds_changed | `text:Recently looked up` | 5 | 1 |
| timing/unslept-step | attribute_placement_unverified | `label:Back` | 4 | 1 |
| timing/unslept-step | attribute_placement_unverified | `label:Skip Anki setup` | 3, 4 | 2 |
| timing/unslept-step | attribute_placement_unverified | `label:Step 1 of 2, Install AnkiDroid, current` | 4 | 1 |
| timing/unslept-step | attribute_placement_unverified | `label:Step 2 of 2, Allow access, not started` | 4 | 1 |
| timing/unslept-step | attribute_placement_unverified | `label:Step 2 of 3` | 2, 3, 4 | 3 |
| timing/unslept-step | attribute_placement_unverified | `label:Study with Anki. Step 2 of 3.` | 4 | 1 |
| timing/unslept-step | attribute_placement_unverified | `text:Skip` | 2, 3, 4 | 3 |
| timing/unslept-step | bounds_changed | `text:Skip` | 2, 3, 4 | 3 |
| timing/unslept-step | label_only_in_acos | `label:2 / 3` | 3, 4 | 2 |
| timing/unslept-step | label_only_in_acos | `label:A separate app from Google Play.` | 4 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Allow access` | 4 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Connect AnkiDroid to review your decks and save words from your lookups as flashcards.` | 4 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Install AnkiDroid` | 2, 3, 4 | 3 |
| timing/unslept-step | label_only_in_acos | `label:Look up Japanese anywhere` | 2, 3 | 2 |
| timing/unslept-step | label_only_in_acos | `label:Look up Japanese words and kanji while you read in other apps. Enable lookup mode to get started.` | 2, 3 | 2 |
| timing/unslept-step | label_only_in_acos | `label:Read decks, record answers, add words.` | 4 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Skip` | 4 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Study with Anki` | 4 | 1 |
| timing/unslept-step | label_only_on_phone | `label:0` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:1` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:3 / 3` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Allow Gaten to read your AnkiDroid collection.` | 2 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Allow access` | 2 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Analytics` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Anki` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Anki Mon: study activity unknown` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Anki Sun: upcoming` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Anki Tue: study activity unknown` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:AnkiDroid is installed` | 2 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Connect` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Connect AnkiDroid to review your decks and save words from your lookups as flashcards.` | 2 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Connect WaniKani` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Connect your WaniKani account to review kanji and vocabulary alongside your other study.` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Create a token` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Daily streak` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Fri` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Gaten` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Home` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Lookup mode off. Open screen lookup settings.` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Mon` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Need a token? Show me how` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:No recent searches found` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Not connected` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Paste it here` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Paste your token` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Read decks, record answers, add words.` | 2 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Recently looked up` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Sat` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Settings` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Show` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Show token` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Skip WaniKani setup and finish` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Step 1 of 2, AnkiDroid is installed, done` | 2 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Step 1 of 2, Create a token, available` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Step 2 of 2, Allow access, current` | 2 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Step 2 of 2, Paste it here, current` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Study with Anki` | 2 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Sun` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Thu` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Tue` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:WaniKani` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:WaniKani, Not connected, Connect` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Wed` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:days` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:猫, ねこ, neko...` | 3, 4 | 2 |

* **attribute_placement_unverified** `attr:accessible-name-placement` (evidence-validity/attribute-placement-unverified): `{"field": "text|content_desc", "reason": "the capture cannot tell a node's text from its content-desc (not read from uiautomator dump); these strings are present on both sides and their attribute is not comparable"}`
* **attribute_placement_unverified** `label:Anki Fri: upcoming` (evidence-validity/attribute-placement-unverified): `{"field": "content_desc", "reason": "ACOS exposes this as a node's content-desc; the capture does not record content-desc (not read from uiautomator dump), so its absence there is not comparable"}`
* **attribute_placement_unverified** `label:Anki Sat: upcoming` (evidence-validity/attribute-placement-unverified): `{"field": "content_desc", "reason": "ACOS exposes this as a node's content-desc; the capture does not record content-desc (not read from uiautomator dump), so its absence there is not comparable"}`
* **attribute_placement_unverified** `label:Anki Thu: upcoming` (evidence-validity/attribute-placement-unverified): `{"field": "content_desc", "reason": "ACOS exposes this as a node's content-desc; the capture does not record content-desc (not read from uiautomator dump), so its absence there is not comparable"}`
* **attribute_placement_unverified** `label:Anki Wed: study activity unknown, today` (evidence-validity/attribute-placement-unverified): `{"field": "content_desc", "reason": "ACOS exposes this as a node's content-desc; the capture does not record content-desc (not read from uiautomator dump), so its absence there is not comparable"}`
* **attribute_placement_unverified** `label:Anki, Not connected, Open Anki` (evidence-validity/attribute-placement-unverified): `{"field": "content_desc", "reason": "ACOS exposes this as a node's content-desc; the capture does not record content-desc (not read from uiautomator dump), so its absence there is not comparable"}`
* **attribute_placement_unverified** `label:Daily streak. Anki study days follow your collection's rollover.` (evidence-validity/attribute-placement-unverified): `{"field": "content_desc", "reason": "ACOS exposes this as a node's content-desc; the capture does not record content-desc (not read from uiautomator dump), so its absence there is not comparable"}`
* **attribute_placement_unverified** `label:Look up Japanese anywhere. Step 1 of 3.` (evidence-validity/attribute-placement-unverified): `{"field": "content_desc", "reason": "ACOS exposes this as a node's content-desc; the capture does not record content-desc (not read from uiautomator dump), so its absence there is not comparable"}`

## Documented platform differences (allowlisted)

None.

## Settings

```json
{
  "allowlist": "tools/parity/allowlist.yaml",
  "bounds_normalisation": "px * 160 / dpi, per side",
  "bounds_tolerance_dp": 1.5,
  "strict_class": false
}
```
