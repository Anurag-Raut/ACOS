### gaten-settings

| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |
|---|---|---|---|---|---|---|---|
| 0 | launch | `-` | pass / - / - | pass / - / - | - | Gaten looks up words you circle on screen and saves them to Anki., Look up Japanese in any | **text-set** |
| 1 | tap | `{"text": {"contains": "Set up Gaten"}}` | pass / - / - | pass / Set up Gaten / [55,2130][1025,2260] | Look up Japanese anywhere. Step 1 of 3., Step 1 of 3 | - | **text-set** |
| 2 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2216] | pass / Skip / [500,2175][580,2225] | 1 / 3, Android needs Gaten as an accessibility service., Choose text recognition, Look up  | 2 / 3, Allow Gaten to read your AnkiDroid collection., Allow access, AnkiDroid is installe | **bounds** |
| 3 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2216] | pass / Skip / [500,2175][580,2225] | 2 / 3, A separate app from Google Play., Allow access, Connect AnkiDroid to review your de | 0, 1, 3 / 3, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity u | **bounds** |
| 4 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2216] | pass / Skip / [500,2175][580,2225] | 3 / 3, Back, Connect AnkiDroid to review your decks and save words from your lookups as fl | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | **bounds** |
| 5 | wait_for | `{"text": {"contains": "Recently looked up"}}` | pass / Recently looked up / [58,1424][1024,1482] | pass / Recently looked up / [55,1343][1025,1398] | Anki, Not connected, Open Anki, Daily streak. Anki study days follow your collection's rol | Anki, Permission needed, Grant access, Grant access, Permission needed | **bounds** |
| 6 | capture | `-` | pass / - / - | pass / - / - | Anki, Not connected, Open Anki, Daily streak. Anki study days follow your collection's rol | Anki, Permission needed, Grant access, Grant access, Permission needed | **text-set** |
| 7 | tap | `{"content_desc": {"exact": "Settings"}}` | pass / Settings / [58,188][1022,304] | pass / Settings / [682,2163][847,2320] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 8 | wait_for | `{"text": {"exact": "Screen lookup"}}` | pass / Screen lookup / [147,483][872,536] | pass / Screen lookup / [140,446][883,496] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 9 | capture | `-` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 10 | assert_text | `{"text": {"exact": "Screen lookup"}}` | pass / Screen lookup / [147,483][872,536] | pass / Screen lookup / [140,446][883,496] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 11 | assert_text | `{"text": {"exact": "Kanji reference"}}` | pass / Kanji reference / [153,1250][557,1303] | pass / Kanji reference / [145,1176][587,1226] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 12 | assert_text | `{"text": {"exact": "Appearance"}}` | pass / Appearance / [147,1537][843,1590] | pass / Appearance / [140,1448][855,1498] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 13 | tap | `{"content_desc": {"contains": "Appearance."}}` | pass / - / - | pass / Appearance. Light / [55,1398][1025,1548] | - | - | **match** |
| 14 | wait_for | `{"text": {"contains": "Accent"}}` | pass / Accent / [58,898][1024,946] | pass / Accent / [55,841][1025,886] | - | - | **bounds** |
| 15 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 16 | assert_text | `{"text": {"exact": "Theme"}}` | pass / Theme / [58,317][1024,365] | pass / Theme / [55,288][1025,333] | - | - | **bounds** |
| 17 | assert_text | `{"text": {"exact": "Haptics"}}` | pass / Haptics / [152,1591][907,1644] | pass / Haptics / [145,1498][917,1548] | - | - | **bounds** |
| 18 | tap | `{"content_desc": {"exact": "Theme Dark"}}` | pass / Theme Dark / [58,685][1022,832] | pass / Theme Dark / [55,638][1025,778] | - | - | **bounds** |
| 19 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 20 | back | `-` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 21 | wait_for | `{"text": {"exact": "Screen lookup"}}` | pass / Screen lookup / [147,483][872,536] | pass / Screen lookup / [140,446][883,496] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 22 | capture | `-` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 23 | assert_text | `{"text": {"exact": "Dark"}}` | pass / Dark / [877,1539][951,1587] | pass / Dark / [887,1450][957,1495] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 24 | tap | `{"content_desc": {"contains": "About."}}` | pass / - / - | pass / About. 0.1.0 / [55,1703][1025,1853] | - | Gaten is free software under GPL-3.0-or-later, derived from CircleToSearch by AKS-Labs., L | **text-set** |
| 25 | wait_for | `{"text": {"contains": "Privacy"}}` | pass / Privacy / [58,515][1024,563] | pass / Privacy / [55,476][1025,521] | - | Gaten is free software under GPL-3.0-or-later, derived from CircleToSearch by AKS-Labs., L | **bounds** |
| 26 | capture | `-` | pass / - / - | pass / - / - | - | Gaten is free software under GPL-3.0-or-later, derived from CircleToSearch by AKS-Labs., L | **text-set** |
| 27 | assert_text | `{"text": {"contains": "Japanese lookup, offline by default."}}` | pass / Version 0.1.0 · Japanese lookup, offline by default. / [58,396][1024,441] | pass / Version 0.1.0 · Japanese lookup, offline by default. / [55,363][1025,406] | - | Gaten is free software under GPL-3.0-or-later, derived from CircleToSearch by AKS-Labs., L | **bounds** |
| 28 | assert_text | `{"text": {"exact": "Privacy policy"}}` | pass / Privacy policy / [153,907][953,960] | pass / Privacy policy / [145,849][958,899] | - | Gaten is free software under GPL-3.0-or-later, derived from CircleToSearch by AKS-Labs., L | **bounds** |
| 29 | back | `-` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 30 | wait_for | `{"text": {"exact": "Screen lookup"}}` | pass / Screen lookup / [147,483][872,536] | pass / Screen lookup / [140,446][883,496] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 31 | capture | `-` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 32 | assert_text | `{"text": {"exact": "About"}}` | pass / About / [147,1857][839,1910] | pass / About / [140,1753][852,1803] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |

Counts: bounds=20, match=3, text-set=10
