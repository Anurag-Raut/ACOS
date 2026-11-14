### gaten-kanji

| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |
|---|---|---|---|---|---|---|---|
| 0 | launch | `-` | pass / - / - | pass / - / - | - | Gaten looks up words you circle on screen and saves them to Anki., Look up Japanese in any | **text-set** |
| 1 | tap | `{"text": {"contains": "Set up Gaten"}}` | pass / - / - | pass / Set up Gaten / [55,2130][1025,2260] | Look up Japanese anywhere. Step 1 of 3., Step 1 of 3 | - | **text-set** |
| 2 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2216] | pass / Skip / [500,2175][580,2225] | Install AnkiDroid, Look up Japanese anywhere, Look up Japanese anywhere. Step 1 of 3., Loo | Allow Gaten to read your AnkiDroid collection., Allow access, AnkiDroid is installed, Conn | **bounds** |
| 3 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2216] | pass / Skip / [500,2175][580,2225] | 2 / 3, A separate app from Google Play., Allow access, Connect AnkiDroid to review your de | 0, 1, 3 / 3, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity u | **bounds** |
| 4 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2216] | pass / Skip / [500,2175][580,2225] | 1, 3 / 3, Back, Connect WaniKani, Connect WaniKani. Step 3 of 3., Connect your WaniKani ac | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | **bounds** |
| 5 | wait_for | `{"text": {"contains": "Recently looked up"}}` | pass / Recently looked up / [58,1424][1024,1482] | pass / Recently looked up / [55,1343][1025,1398] | Anki, Not connected, Open Anki, Daily streak. Anki study days follow your collection's rol | Anki, Permission needed, Grant access, Grant access, Permission needed | **bounds** |
| 6 | capture | `-` | pass / - / - | pass / - / - | Anki, Not connected, Open Anki, Daily streak. Anki study days follow your collection's rol | Anki, Permission needed, Grant access, Grant access, Permission needed | **text-set** |
| 7 | tap | `{"content_desc": {"exact": "Settings"}}` | pass / Settings / [58,188][1022,304] | pass / Settings / [682,2163][847,2320] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 8 | wait_for | `{"text": {"exact": "Screen lookup"}}` | pass / Screen lookup / [147,483][872,536] | pass / Screen lookup / [140,446][883,496] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 9 | capture | `-` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 10 | assert_visible | `{"content_desc": {"exact": "Kanji reference. Remembering the Kanji"}}` | pass / Kanji reference. Remembering the Kanji / [58,1198][1022,1355] | pass / Kanji reference. Remembering the Kanji / [55,1126][1025,1276] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 11 | assert_not_visible | `{"content_desc": {"exact": "Kanji reference. WaniKani"}}` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 12 | tap | `{"content_desc": {"contains": "Kanji reference."}}` | pass / - / - | pass / Kanji reference. Remembering the Kanji / [55,1126][1025,1276] | - | - | **match** |
| 13 | wait_for | `{"text": {"exact": "Remembering the Kanji"}}` | pass / Remembering the Kanji / [58,637][934,690] | pass / Remembering the Kanji / [55,592][940,642] | - | - | **bounds** |
| 14 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 15 | assert_text | `{"text": {"contains": "bundled, works offline"}}` | pass / Remembering the Kanji. Keywords, Heisig stories and Koohii mnemonics · bundled, works offline / [58,605][1022,822] | pass / Remembering the Kanji. Keywords, Heisig stories and Koohii mnemonics · bundled, works offline / [55,562][1025,768] | - | - | **state** |
| 16 | assert_text | `{"text": {"exact": "WaniKani"}}` | pass / WaniKani / [58,418][934,471] | pass / WaniKani / [55,383][940,433] | - | - | **bounds** |
| 17 | tap | `{"content_desc": {"contains": "WaniKani. Your WaniKani account"}}` | pass / WaniKani. Your WaniKani account's kanji pages and radicals · needs a connected account / [58,386][1022,603] | pass / WaniKani. Your WaniKani account's kanji pages and radicals · needs a connected account / [55,353][1025,559] | - | - | **bounds** |
| 18 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 19 | assert_not_visible | `{"text": {"exact": "The choice could not be saved. Try again."}}` | pass / - / - | pass / - / - | - | - | **match** |
| 20 | back | `-` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 21 | wait_for | `{"text": {"exact": "Screen lookup"}}` | pass / Screen lookup / [147,483][872,536] | pass / Screen lookup / [140,446][883,496] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 22 | capture | `-` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 23 | assert_visible | `{"content_desc": {"exact": "Kanji reference. WaniKani"}}` | pass / Kanji reference. WaniKani / [58,1198][1022,1355] | pass / Kanji reference. WaniKani / [55,1126][1025,1276] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 24 | assert_not_visible | `{"content_desc": {"exact": "Kanji reference. Remembering the Kanji"}}` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 25 | tap | `{"content_desc": {"contains": "Kanji reference."}}` | pass / - / - | pass / Kanji reference. WaniKani / [55,1126][1025,1276] | - | - | **match** |
| 26 | wait_for | `{"text": {"exact": "WaniKani"}}` | pass / WaniKani / [58,418][934,471] | pass / WaniKani / [55,383][940,433] | - | - | **bounds** |
| 27 | tap | `{"content_desc": {"contains": "Remembering the Kanji. Keywords"}}` | pass / Remembering the Kanji. Keywords, Heisig stories and Koohii mnemonics · bundled, works offline / [58,605][1022,822] | pass / Remembering the Kanji. Keywords, Heisig stories and Koohii mnemonics · bundled, works offline / [55,562][1025,768] | - | - | **bounds** |
| 28 | back | `-` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 29 | wait_for | `{"text": {"exact": "Screen lookup"}}` | pass / Screen lookup / [147,483][872,536] | pass / Screen lookup / [140,446][883,496] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 30 | capture | `-` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 31 | assert_visible | `{"content_desc": {"exact": "Kanji reference. Remembering the Kanji"}}` | pass / Kanji reference. Remembering the Kanji / [58,1198][1022,1355] | pass / Kanji reference. Remembering the Kanji / [55,1126][1025,1276] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 32 | assert_not_visible | `{"content_desc": {"exact": "Kanji reference. WaniKani"}}` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |

Counts: bounds=16, match=5, state=1, text-set=11
