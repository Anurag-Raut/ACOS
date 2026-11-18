# Parity: gaten-home

* generated: 2026-11-18T08:54:56+00:00
* scenario: `scenarios/gaten-home.yaml` (sha256 `4f1cc247edef`)
* ACOS: `/home/Qui/.t3/worktrees/acos/t3-0a3f7c33-cca21cd9449033f1/.cache/pf/parity/gaten-home` (art / pass, 1080x2400 @400 dpi)
* phone: **recorded-observations** (physical-device (recorded capture)), coverage **text-set+target**, 1080x2400 @400 dpi, `results/phone/gaten-f344272/gaten-home/phone`
* bounds tolerance: 1.5 dp (bounds converted with each side's own density)

## Summary

* steps compared: **13/14** (1 without an ACOS tree)
* matched nodes: **211**, exact matches: **0**
* differences: **266** (0 documented platform differences, **266 unexpected**) in **107 findings**

> The phone was not attached for this run: the comparison uses the checked-in physical-device capture below. It is real moto g35 data, but it is not a fresh replay; re-run with the phone attached to refresh it.
> This capture does not say it was read from `uiautomator dump` (a11y_source=None); none of its steps has a single `text` node, so every name was filed under content-desc by the agent-device mapping that results/handoff/TREE1.md found wrong. Whether a name is the text or the content-desc of its node is therefore not comparable here: it is reported as `attribute_placement_unverified` (needs a live phone replay), never as an ACOS finding and never as a pass.

## Per step

| # | step | ACOS frame | nodes A/P | matched | exact | allowed | unexpected |
|---|---|---|---|---|---|---|---|
| 0 | `launch com.gaten.app.MainActivity` | - | - | - | - | - | no_acos_tree |
| 1 | `tap {"text":{"contains":"Set up Gaten"}}` | 3 (own) | 175/61 | 11 | 0 | 0 | 4 |
| 2 | `tap {"text":{"exact":"Skip"}}` | 4 (own) | 162/66 | 4 | 0 | 0 | 16 |
| 3 | `tap {"text":{"exact":"Skip"}}` | 5 (own) | 162/225 | 2 | 0 | 0 | 61 |
| 4 | `tap {"text":{"exact":"Skip"}}` | 6 (own) | 179/165 | 0 | 0 | 0 | 44 |
| 5 | `wait_for {"text":{"contains":"Recently looked up"}}` | 7 (own) | 218/165 | 26 | 0 | 0 | 18 |
| 6 | `capture gaten-home` | 8 (own) | 218/165 | 26 | 0 | 0 | 16 |
| 7 | `assert_text {"text":{"contains":"Daily streak"}}` | 8 (own) | 218/165 | 26 | 0 | 0 | 18 |
| 8 | `assert_text {"text":{"contains":"Recently looked up"}}` | 8 (own) | 218/165 | 26 | 0 | 0 | 18 |
| 9 | `assert_text {"text":{"exact":"Anki"}}` | 8 (own) | 218/165 | 26 | 0 | 0 | 18 |
| 10 | `tap {"content_desc":{"exact":"Analytics"}}` | 10 (own) | 473/153 | 16 | 0 | 0 | 13 |
| 11 | `wait_for {"text":{"contains":"Accuracy"}}` | 11 (own) | 473/153 | 16 | 0 | 0 | 14 |
| 12 | `capture gaten-analytics` | 12 (own) | 473/153 | 16 | 0 | 0 | 12 |
| 13 | `assert_text {"text":{"contains":"Study activity"}}` | 12 (own) | 473/153 | 16 | 0 | 0 | 14 |

## Findings (unexpected differences)

| classification | kind | node | steps | # |
|---|---|---|---|---|
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `attr:accessible-name-placement` | 1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13 | 12 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `desc:Anki, Collection, Anki is not installed., Open the Anki installation flow., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Study history will appear here., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here.` | 11, 13 | 2 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Anki Fri: upcoming` | 5, 6, 7, 8, 9 | 5 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Anki Sat: upcoming` | 5, 6, 7, 8, 9 | 5 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Anki Thu: upcoming` | 5, 6, 7, 8, 9 | 5 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Anki Wed: study activity unknown, today` | 5, 6, 7, 8, 9 | 5 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Anki, Collection, Anki is not installed., Open the Anki installation flow., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Study history will appear here., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here.` | 10, 11, 12, 13 | 4 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Anki, Not connected, Open Anki` | 5, 6, 7, 8, 9 | 5 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Daily streak. Anki study days follow your collection's rollover.` | 5, 6, 7, 8, 9 | 5 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Look up Japanese anywhere. Step 1 of 3.` | 1, 2 | 2 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Search Japanese dictionary` | 5, 6, 7, 8, 9 | 5 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `label:Step 1 of 3` | 1 | 1 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `text:Anki` | 9 | 1 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `text:Daily streak` | 7 | 1 |
| evidence-validity/attribute-placement-unverified | attribute_placement_unverified | `text:Recently looked up` | 5, 8 | 2 |
| labels/only-in-acos | label_only_in_acos | `label:Anki is not installed.` | 10, 11, 12, 13 | 4 |
| labels/only-in-acos | label_only_in_acos | `label:Install` | 10, 11, 12, 13 | 4 |
| labels/only-in-acos | label_only_in_acos | `label:Open Anki` | 5, 6, 7, 8, 9 | 5 |
| labels/only-in-acos | label_only_in_acos | `label:Open the Anki installation flow.` | 10, 11, 12, 13 | 4 |
| labels/only-in-acos | label_only_in_acos | `label:Study history will appear here.` | 10, 11, 12, 13 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Allow Gaten to read collection statistics.` | 10, 11, 12, 13 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Anki Fri: study activity unknown` | 3, 4, 5, 6, 7, 8, 9 | 7 |
| labels/only-on-phone | label_only_on_phone | `label:Anki Sat: study activity unknown, today` | 3, 4, 5, 6, 7, 8, 9 | 7 |
| labels/only-on-phone | label_only_on_phone | `label:Anki Thu: study activity unknown` | 3, 4, 5, 6, 7, 8, 9 | 7 |
| labels/only-on-phone | label_only_on_phone | `label:Anki Wed: study activity unknown` | 3, 4, 5, 6, 7, 8, 9 | 7 |
| labels/only-on-phone | label_only_on_phone | `label:Anki access needed.` | 10, 11, 12, 13 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Anki, Collection, Anki access needed., Allow Gaten to read collection statistics., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Waiting for permission., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here.` | 10, 11, 12, 13 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Anki, Permission needed, Grant access` | 3, 4, 5, 6, 7, 8, 9 | 7 |
| labels/only-on-phone | label_only_on_phone | `label:Grant access` | 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13 | 11 |
| labels/only-on-phone | label_only_on_phone | `label:Permission needed` | 3, 4, 5, 6, 7, 8, 9 | 7 |
| labels/only-on-phone | label_only_on_phone | `label:Waiting for permission.` | 10, 11, 12, 13 | 4 |
| labels/only-on-phone | label_only_on_phone | `label:Your level history will appear here.` | 10, 11, 12, 13 | 4 |
| layout/bounds | bounds_changed | `desc:Analytics` | 10 | 1 |
| layout/bounds | bounds_changed | `desc:Anki, Collection, Anki is not installed., Open the Anki installation flow., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Study history will appear here., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here.` | 11, 13 | 2 |
| layout/bounds | bounds_changed | `desc:Set up Gaten` | 1 | 1 |
| layout/bounds | bounds_changed | `text:Anki` | 9 | 1 |
| layout/bounds | bounds_changed | `text:Daily streak` | 7 | 1 |
| layout/bounds | bounds_changed | `text:Recently looked up` | 5, 8 | 2 |
| timing/unslept-step | attribute_placement_unverified | `label:Back` | 4 | 1 |
| timing/unslept-step | attribute_placement_unverified | `label:Skip Anki setup` | 3 | 1 |
| timing/unslept-step | attribute_placement_unverified | `label:Skip WaniKani setup and finish` | 4 | 1 |
| timing/unslept-step | attribute_placement_unverified | `label:Step 1 of 2, Install AnkiDroid, current` | 3 | 1 |
| timing/unslept-step | attribute_placement_unverified | `label:Step 2 of 2, Allow access, not started` | 3 | 1 |
| timing/unslept-step | attribute_placement_unverified | `label:Step 2 of 3` | 2, 3 | 2 |
| timing/unslept-step | attribute_placement_unverified | `label:Step 3 of 3` | 4 | 1 |
| timing/unslept-step | attribute_placement_unverified | `label:Study with Anki. Step 2 of 3.` | 3, 4 | 2 |
| timing/unslept-step | attribute_placement_unverified | `text:Skip` | 2, 3, 4 | 3 |
| timing/unslept-step | bounds_changed | `text:Skip` | 2, 3, 4 | 3 |
| timing/unslept-step | label_only_in_acos | `label:2 / 3` | 3 | 1 |
| timing/unslept-step | label_only_in_acos | `label:3 / 3` | 4 | 1 |
| timing/unslept-step | label_only_in_acos | `label:A separate app from Google Play.` | 3 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Allow access` | 3 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Connect AnkiDroid to review your decks and save words from your lookups as flashcards.` | 3, 4 | 2 |
| timing/unslept-step | label_only_in_acos | `label:Connect WaniKani` | 4 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Install AnkiDroid` | 2, 3 | 2 |
| timing/unslept-step | label_only_in_acos | `label:Look up Japanese anywhere` | 2 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Look up Japanese words and kanji while you read in other apps. Enable lookup mode to get started.` | 2 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Read decks, record answers, add words.` | 3 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Skip` | 4 | 1 |
| timing/unslept-step | label_only_in_acos | `label:Study with Anki` | 3, 4 | 2 |
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
* **attribute_placement_unverified** `desc:Anki, Collection, Anki is not installed., Open the Anki installation flow., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Study history will appear here., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here.` (evidence-validity/attribute-placement-unverified): `{"field": "content_desc", "acos": "Anki, Collection, Anki is not installed., Open the Anki installation flow., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Study history will appear here., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here.", "phone": "Anki, Collection, Anki access needed., Allow Gaten to read collection statistics., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Waiting for permission., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here."}`
* **attribute_placement_unverified** `label:Anki Fri: upcoming` (evidence-validity/attribute-placement-unverified): `{"field": "content_desc", "reason": "ACOS exposes this as a node's content-desc; the capture does not record content-desc (not read from uiautomator dump), so its absence there is not comparable"}`
* **attribute_placement_unverified** `label:Anki Sat: upcoming` (evidence-validity/attribute-placement-unverified): `{"field": "content_desc", "reason": "ACOS exposes this as a node's content-desc; the capture does not record content-desc (not read from uiautomator dump), so its absence there is not comparable"}`
* **attribute_placement_unverified** `label:Anki Thu: upcoming` (evidence-validity/attribute-placement-unverified): `{"field": "content_desc", "reason": "ACOS exposes this as a node's content-desc; the capture does not record content-desc (not read from uiautomator dump), so its absence there is not comparable"}`
* **attribute_placement_unverified** `label:Anki Wed: study activity unknown, today` (evidence-validity/attribute-placement-unverified): `{"field": "content_desc", "reason": "ACOS exposes this as a node's content-desc; the capture does not record content-desc (not read from uiautomator dump), so its absence there is not comparable"}`
* **attribute_placement_unverified** `label:Anki, Collection, Anki is not installed., Open the Anki installation flow., WaniKani, Progress, Not connected, Connect to see WaniKani progress., Study activity, 12 weeks · Anki, Study history will appear here., Accuracy, WaniKani, Answer accuracy will appear here., Level progress, WaniKani, Your level history will appear here.` (evidence-validity/attribute-placement-unverified): `{"field": "content_desc", "reason": "ACOS exposes this as a node's content-desc; the capture does not record content-desc (not read from uiautomator dump), so its absence there is not comparable"}`
* **attribute_placement_unverified** `label:Anki, Not connected, Open Anki` (evidence-validity/attribute-placement-unverified): `{"field": "content_desc", "reason": "ACOS exposes this as a node's content-desc; the capture does not record content-desc (not read from uiautomator dump), so its absence there is not comparable"}`

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
