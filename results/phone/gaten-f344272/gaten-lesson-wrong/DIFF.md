### gaten-lesson-wrong

| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |
|---|---|---|---|---|---|---|---|
| 0 | launch | `-` | pass / - / - | pass / - / - | - | Gaten looks up words you circle on screen and saves them to Anki., Look up Japanese in any | **text-set** |
| 1 | tap | `{"text": {"contains": "Set up Gaten"}}` | pass / - / - | pass / Set up Gaten / [55,2130][1025,2260] | Look up Japanese anywhere. Step 1 of 3., Step 1 of 3 | - | **text-set** |
| 2 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2216] | pass / Skip / [500,2175][580,2225] | Install AnkiDroid, Look up Japanese anywhere, Look up Japanese anywhere. Step 1 of 3., Loo | Allow Gaten to read your AnkiDroid collection., Allow access, AnkiDroid is installed, Conn | **bounds** |
| 3 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2216] | pass / Skip / [500,2175][580,2225] | 2 / 3, A separate app from Google Play., Allow access, Connect AnkiDroid to review your de | 0, 1, 3 / 3, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity u | **bounds** |
| 4 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2216] | pass / Skip / [500,2175][580,2225] | 1, 3 / 3, Back, Connect WaniKani, Connect WaniKani. Step 3 of 3., Connect your WaniKani ac | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | **bounds** |
| 5 | wait_for | `{"text": {"contains": "Recently looked up"}}` | pass / Recently looked up / [58,1424][1024,1482] | pass / Recently looked up / [55,1343][1025,1398] | Anki, Not connected, Open Anki, Daily streak. Anki study days follow your collection's rol | Anki, Permission needed, Grant access, Grant access, Permission needed | **bounds** |
| 6 | capture | `-` | pass / - / - | pass / - / - | Anki, Not connected, Open Anki, Daily streak. Anki study days follow your collection's rol | Anki, Permission needed, Grant access, Grant access, Permission needed | **text-set** |
| 7 | tap | `{"content_desc": {"contains": "Anki, Not connected"}}` | pass / - / - | fail / - / - | Anki is not installed, Anki setup help, Back, Download Anki, Install Anki and finish setup | 0, Analytics, Anki Fri: study activity unknown, Anki Mon: study activity unknown, Anki Sat | **status** |
| 8 | wait_for | `{"text": {"exact": "Anki is not installed"}}` | pass / Anki is not installed / [278,1150][801,1224] | fail / - / - | Anki is not installed, Anki setup help, Back, Download Anki, Install Anki and finish setup | 0, Analytics, Anki Fri: study activity unknown, Anki Mon: study activity unknown, Anki Sat | **missing-on-phone** |
| 9 | capture | `-` | pass / - / - | pass / - / - | Anki is not installed, Anki setup help, Back, Download Anki, Install Anki and finish setup | 0, Analytics, Anki Fri: study activity unknown, Anki Mon: study activity unknown, Anki Sat | **text-set** |
| 10 | assert_text | `{"text": {"exact": "Anki is not installed"}}` | fail / Anki is not installed / [278,1150][801,1224] | fail / - / - | Anki is not installed, Anki setup help, Back, Download Anki, Install Anki and finish setup | 0, Analytics, Anki Fri: study activity unknown, Anki Mon: study activity unknown, Anki Sat | **text-set** |
| 11 | assert_text | `{"text": {"exact": "Download Anki"}}` | - / - / - | fail / - / - | - | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | **missing-side** |
| 12 | back | `-` | - / - / - | pass / - / - | - | - | **missing-side** |
| 13 | wait_for | `{"text": {"contains": "Recently looked up"}}` | - / - / - | fail / - / - | - | - | **missing-side** |
| 14 | capture | `-` | - / - / - | pass / - / - | - | - | **missing-side** |
| 15 | tap | `{"content_desc": {"contains": "WaniKani, Not connected"}}` | - / - / - | fail / - / - | - | - | **missing-side** |
| 16 | wait_for | `{"text": {"exact": "Connect WaniKani"}}` | - / - / - | fail / - / - | - | - | **missing-side** |
| 17 | capture | `-` | - / - / - | pass / - / - | - | - | **missing-side** |
| 18 | assert_text | `{"text": {"exact": "Connect WaniKani"}}` | - / - / - | fail / - / - | - | - | **missing-side** |
| 19 | assert_text | `{"text": {"exact": "Personal access token"}}` | - / - / - | fail / - / - | - | - | **missing-side** |
| 20 | tap | `{"content_desc": {"exact": "WaniKani personal access token"}}` | - / - / - | fail / - / - | - | - | **missing-side** |
| 21 | type | `-` | - / - / - | pass / - / - | - | - | **missing-side** |
| 22 | capture | `-` | - / - / - | pass / - / - | - | - | **missing-side** |
| 23 | assert_text | `{"content_desc": {"exact": "WaniKani personal access token"}}` | - / - / - | fail / - / - | - | - | **missing-side** |
| 24 | back | `-` | - / - / - | pass / - / - | - | - | **missing-side** |
| 25 | wait_for | `{"text": {"contains": "Recently looked up"}}` | - / - / - | fail / - / - | - | - | **missing-side** |
| 26 | capture | `-` | - / - / - | pass / - / - | - | - | **missing-side** |
| 27 | assert_text | `{"text": {"contains": "Daily streak"}}` | - / - / - | fail / - / - | - | - | **missing-side** |

Counts: bounds=4, missing-on-phone=1, missing-side=17, status=1, text-set=5
