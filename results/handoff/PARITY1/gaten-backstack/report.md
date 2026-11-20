# Parity: gaten-backstack

* generated: 2026-11-18T08:37:41+00:00
* scenario: `scenarios/gaten-backstack.yaml` (sha256 `a3389de15c41`)
* ACOS: `.cache/parity1/runs/gaten-backstack` (art / pass, 1080x2400 @400 dpi)
* phone: **recorded-observations** (physical-device (recorded capture)), coverage **text-set+target**, 1080x2400 @400 dpi, `results/phone/gaten-f344272/gaten-backstack/phone`
* bounds tolerance: 1.5 dp (bounds converted with each side's own density)

## Summary

* steps compared: **24/25** (1 without an ACOS tree)
* matched nodes: **326**, exact matches: **0**
* differences: **236** (0 documented platform differences, **236 unexpected**) in **108 findings**
* excluded as invalid evidence: 100 differences on steps [13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24] (phone replay failed earlier)

> **Phone replay failed at steps [13, 15].** Past the first failure the phone is not on the screen the scenario asked for; those differences are classified `phone-scenario-failed` and say nothing about ACOS.

> The phone was not attached for this run: the comparison uses the checked-in physical-device capture below. It is real moto g35 data, but it is not a fresh replay; re-run with the phone attached to refresh it.

## Per step

| # | step | ACOS frame | nodes A/P | matched | exact | allowed | unexpected |
|---|---|---|---|---|---|---|---|
| 0 | `launch com.gaten.app.MainActivity` | - | - | - | - | - | no_acos_tree |
| 1 | `tap {"text":{"contains":"Set up Gaten"}}` | 3 (own) | 175/61 | 11 | 0 | 0 | 4 |
| 2 | `tap {"text":{"exact":"Skip"}}` | 4 (own) | 175/66 | 1 | 0 | 0 | 26 |
| 3 | `tap {"text":{"exact":"Skip"}}` | 5 (own) | 162/225 | 2 | 0 | 0 | 56 |
| 4 | `tap {"text":{"exact":"Skip"}}` | 6 (own) | 162/165 | 0 | 0 | 0 | 44 |
| 5 | `wait_for {"text":{"contains":"Recently looked up"}}` | 7 (own) | 218/165 | 26 | 0 | 0 | 18 |
| 6 | `capture backstack-home` | 8 (own) | 218/165 | 26 | 0 | 0 | 16 |
| 7 | `tap {"content_desc":{"exact":"Analytics"}}` | 10 (own) | 473/153 | 16 | 0 | 0 | 13 |
| 8 | `wait_for {"text":{"contains":"Study activity"}}` | 11 (own) | 473/153 | 16 | 0 | 0 | 14 |
| 9 | `capture backstack-analytics` | 12 (own) | 473/153 | 16 | 0 | 0 | 12 |
| 10 | `assert_text {"text":{"exact":"Not connected"}}` | 12 (own) | 473/153 | 16 | 0 | 0 | 14 |
| 11 | `assert_text {"text":{"contains":"Connect to see WaniKani progress"}}` | 12 (own) | 473/153 | 16 | 0 | 0 | 14 |
| 12 | `tap {"text":{"exact":"Connect"}}` | 13 (own) | 378/48 | 12 | 0 | 0 | 5 |
| 13 | `wait_for {"content_desc":{"exact":"Personal access token"}}` | 14 (own) | 378/48 | 12 | 0 | invalid | invalid |
| 14 | `capture backstack-wanikani-page` | 15 (own) | 378/48 | 12 | 0 | invalid | invalid |
| 15 | `assert_text {"content_desc":{"exact":"Personal access token"}}` | 15 (own) | 378/48 | 12 | 0 | invalid | invalid |
| 16 | `assert_text {"text":{"exact":"New per day"}}` | 15 (own) | 378/48 | 12 | 0 | invalid | invalid |
| 17 | `assert_text {"text":{"exact":"Not connected"}}` | 15 (own) | 378/48 | 12 | 0 | invalid | invalid |
| 18 | `assert_visible {"text":{"exact":"Save"}}` | 15 (own) | 378/48 | 12 | 0 | invalid | invalid |
| 19 | `back` | 16 (own) | 473/153 | 16 | 0 | invalid | invalid |
| 20 | `wait_for {"text":{"contains":"Study activity"}}` | 17 (own) | 473/153 | 16 | 0 | invalid | invalid |
| 21 | `capture backstack-analytics-again` | 18 (own) | 473/153 | 16 | 0 | invalid | invalid |
| 22 | `assert_text {"text":{"exact":"Accuracy"}}` | 18 (own) | 473/153 | 16 | 0 | invalid | invalid |
| 23 | `assert_text {"text":{"exact":"Answer accuracy will appear here."}}` | 18 (own) | 473/153 | 16 | 0 | invalid | invalid |
| 24 | `assert_not_visible {"text":{"exact":"Kanji reference"}}` | 18 (own) | 473/153 | 16 | 0 | invalid | invalid |

## Findings (unexpected differences)

| classification | kind | node | steps | # |
|---|---|---|---|---|
| compatibility-bug/accessibility-name | text_vs_desc | `text:Connect` | 12 | 1 |
| compatibility-bug/accessibility-name | text_vs_desc | `text:Not connected` | 10 | 1 |
| compatibility-bug/accessibility-name | text_vs_desc | `text:Recently looked up` | 5 | 1 |
| compatibility-bug/accessibility-name-attribute | accessibility_name_attribute | `attr:accessible-name-placement` | 1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12 | 11 |
| compatibility-bug/content-desc | desc_changed | `desc:Anki, Collection, Anki is not installed., Open the Anki installation flow., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Study history will appear here., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here.` | 8, 11 | 2 |
| labels/only-in-acos | label_only_in_acos | `label:Anki Fri: upcoming` | 5, 6 | 2 |
| labels/only-in-acos | label_only_in_acos | `label:Anki Sat: upcoming` | 5, 6 | 2 |
| labels/only-in-acos | label_only_in_acos | `label:Anki Thu: upcoming` | 5, 6 | 2 |
| labels/only-in-acos | label_only_in_acos | `label:Anki Wed: study activity unknown, today` | 5, 6 | 2 |
| labels/only-in-acos | label_only_in_acos | `label:Anki is not installed.` | 7, 8, 9, 10, 11 | 5 |
| labels/only-in-acos | label_only_in_acos | `label:Anki, Collection, Anki is not installed., Open the Anki installation flow., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Study history will appear here., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here.` | 7, 8, 9, 10, 11 | 5 |
| labels/only-in-acos | label_only_in_acos | `label:Anki, Not connected, Open Anki` | 5, 6 | 2 |
| labels/only-in-acos | label_only_in_acos | `label:Daily streak. Anki study days follow your collection's rollover.` | 5, 6 | 2 |
| labels/only-in-acos | label_only_in_acos | `label:Install` | 7, 8, 9, 10, 11 | 5 |
| labels/only-in-acos | label_only_in_acos | `label:Look up Japanese anywhere. Step 1 of 3.` | 1, 2, 3, 4 | 4 |
| labels/only-in-acos | label_only_in_acos | `label:New per day. A whole number from 1 to 20.` | 12 | 1 |
| labels/only-in-acos | label_only_in_acos | `label:Open Anki` | 5, 6 | 2 |
| labels/only-in-acos | label_only_in_acos | `label:Open the Anki installation flow.` | 7, 8, 9, 10, 11 | 5 |
| labels/only-in-acos | label_only_in_acos | `label:Personal access token` | 12 | 1 |
| labels/only-in-acos | label_only_in_acos | `label:Search Japanese dictionary` | 5, 6 | 2 |
| labels/only-in-acos | label_only_in_acos | `label:Step 1 of 3` | 1, 2 | 2 |
| labels/only-in-acos | label_only_in_acos | `label:Study history will appear here.` | 7, 8, 9, 10, 11 | 5 |
| labels/only-on-phone | label_only_on_phone | `label:Allow Gaten to read collection statistics.` | 7, 8, 9, 10, 11 | 5 |
| labels/only-on-phone | label_only_on_phone | `label:Anki Fri: study activity unknown` | 3, 4, 5, 6 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Anki Sat: study activity unknown, today` | 3, 4, 5, 6 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Anki Thu: study activity unknown` | 3, 4, 5, 6 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Anki Wed: study activity unknown` | 3, 4, 5, 6 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Anki access needed.` | 7, 8, 9, 10, 11 | 5 |
| labels/only-on-phone | label_only_on_phone | `label:Anki, Collection, Anki access needed., Allow Gaten to read collection statistics., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Waiting for permission., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here.` | 7, 8, 9, 10, 11 | 5 |
| labels/only-on-phone | label_only_on_phone | `label:Anki, Permission needed, Grant access` | 3, 4, 5, 6 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Grant access` | 3, 4, 5, 6, 7, 8, 9, 10, 11 | 9 |
| labels/only-on-phone | label_only_on_phone | `label:Permission needed` | 3, 4, 5, 6 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Waiting for permission.` | 7, 8, 9, 10, 11 | 5 |
| labels/only-on-phone | label_only_on_phone | `label:Your level history will appear here.` | 7, 8, 9, 10, 11 | 5 |
| layout/bounds | bounds_changed | `desc:Analytics` | 7 | 1 |
| layout/bounds | bounds_changed | `desc:Anki, Collection, Anki is not installed., Open the Anki installation flow., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Study history will appear here., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here.` | 8, 11 | 2 |
| layout/bounds | bounds_changed | `desc:Set up Gaten` | 1 | 1 |
| layout/bounds | bounds_changed | `text:Connect` | 12 | 1 |
| layout/bounds | bounds_changed | `text:Not connected` | 10 | 1 |
| layout/bounds | bounds_changed | `text:Recently looked up` | 5 | 1 |
| timing/unslept-step | bounds_changed | `text:Skip` | 2, 3, 4 | 3 |
| timing/unslept-step | label_only_in_acos | `label:1 / 3` | 2 | 1 |
| timing/unslept-step | label_only_in_acos | `label:2 / 3` | 3, 4 | 2 |
| timing/unslept-step | label_only_in_acos | `label:Android needs Gaten as an accessibility service.` | 2 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Back` | 4 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Choose text recognition` | 2 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Install AnkiDroid` | 3, 4 | 2 |
| timing/unslept-step | label_only_in_acos | `label:Look up Japanese anywhere` | 2, 3, 4 | 3 |
| timing/unslept-step | label_only_in_acos | `label:Look up Japanese words and kanji while you read in other apps. Enable lookup mode to get started.` | 2, 3, 4 | 3 |
| timing/unslept-step | label_only_in_acos | `label:Skip` | 4 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Skip Anki setup` | 3, 4 | 2 |
| timing/unslept-step | label_only_in_acos | `label:Skip lookup mode setup` | 2 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Standard or Enhanced` | 2 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Step 1 of 2, Turn on lookup mode, current` | 2 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Step 2 of 2, Choose text recognition, not started` | 2 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Step 2 of 3` | 3, 4 | 2 |
| timing/unslept-step | label_only_in_acos | `label:Turn on lookup mode` | 2 | 1 |
| timing/unslept-step | label_only_on_phone | `label:0` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:1` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:2 / 3` | 2 | 1 |
| timing/unslept-step | label_only_on_phone | `label:3 / 3` | 3 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Allow Gaten to read your AnkiDroid collection.` | 2 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Allow access` | 2 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Analytics` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Anki` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Anki Mon: study activity unknown` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Anki Sun: upcoming` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:Anki Tue: study activity unknown` | 3, 4 | 2 |
| timing/unslept-step | label_only_on_phone | `label:AnkiDroid is installed` | 2 | 1 |
| timing/unslept-step | label_only_on_phone | `label:Back` | 2 | 1 |
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
| timing/unslept-step | label_only_on_phone | `label:Skip Anki setup` | 2 | 1 |
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
| timing/unslept-step | text_vs_desc | `text:Skip` | 2, 3, 4 | 3 |

* **text_vs_desc** `text:Connect` (compatibility-bug/accessibility-name): `{"field": "text->desc", "acos": {"key": "text:Connect", "id": "", "class": "X4.e", "role": "other:e", "text": "Connect", "desc": "", "visible": true, "bounds_px": [342, 1049, 499, 1102], "bounds_dp": [136.8, 419.6, 199.6, 440.8], "depth": 0}, "phone": {"key": "desc:Connect", "id": "", "class": "", "role": "unknown", "text": "", "desc": "Connect", "visible": true, "bounds_px": [325, 949, 525, 1069], "bounds_dp": [130.0, 379.6, 210.0, 427.6], "depth": 0}}`
* **text_vs_desc** `text:Not connected` (compatibility-bug/accessibility-name): `{"field": "text->desc", "acos": {"key": "text:Not connected", "id": "", "class": "X4.e", "role": "other:e", "text": "Not connected", "desc": "", "visible": true, "bounds_px": [376, 899, 623, 949], "bounds_dp": [150.4, 359.6, 249.2, 379.6], "depth": 0}, "phone": {"key": "desc:Not connected", "id": "", "class": "", "role": "unknown", "text": "", "desc": "Not connected", "visible": true, "bounds_px": [357, 841, 590, 889], "bounds_dp": [142.8, 336.4, 236.0, 355.6], "depth": 0}}`
* **text_vs_desc** `text:Recently looked up` (compatibility-bug/accessibility-name): `{"field": "text->desc", "acos": {"key": "text:Recently looked up", "id": "", "class": "X4.e", "role": "other:e", "text": "Recently looked up", "desc": "", "visible": true, "bounds_px": [58, 1424, 1024, 1482], "bounds_dp": [23.2, 569.6, 409.6, 592.8], "depth": 0}, "phone": {"key": "desc:Recently looked up", "id": "", "class": "", "role": "unknown", "text": "", "desc": "Recently looked up", "visible": true, "bounds_px": [55, 1343, 1025, 1398], "bounds_dp": [22.0, 537.2, 410.0, 559.2], "depth": 0}}`
* **accessibility_name_attribute** `attr:accessible-name-placement` (compatibility-bug/accessibility-name-attribute): `{"field": "text|content_desc", "reason": "the same string is a node's text on one side and its content-desc on the other"}`
* **desc_changed** `desc:Anki, Collection, Anki is not installed., Open the Anki installation flow., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Study history will appear here., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here.` (compatibility-bug/content-desc): `{"field": "content_desc", "acos": "Anki, Collection, Anki is not installed., Open the Anki installation flow., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Study history will appear here., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here.", "phone": "Anki, Collection, Anki access needed., Allow Gaten to read collection statistics., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Waiting for permission., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here."}`
* **label_only_in_acos** `label:Anki Fri: upcoming` (labels/only-in-acos): `{"reason": "ACOS shows this text/content-desc; the recorded phone screen did not"}`
* **label_only_in_acos** `label:Anki Sat: upcoming` (labels/only-in-acos): `{"reason": "ACOS shows this text/content-desc; the recorded phone screen did not"}`
* **label_only_in_acos** `label:Anki Thu: upcoming` (labels/only-in-acos): `{"reason": "ACOS shows this text/content-desc; the recorded phone screen did not"}`

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
