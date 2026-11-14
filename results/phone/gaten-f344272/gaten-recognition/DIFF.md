### gaten-recognition

| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |
|---|---|---|---|---|---|---|---|
| 0 | launch | `-` | pass / - / - | pass / - / - | - | Gaten looks up words you circle on screen and saves them to Anki., Look up Japanese in any | **text-set** |
| 1 | tap | `{"text": {"contains": "Set up Gaten"}}` | pass / - / - | pass / Set up Gaten / [55,2130][1025,2260] | Look up Japanese anywhere. Step 1 of 3., Step 1 of 3 | - | **text-set** |
| 2 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2216] | pass / Skip / [500,2175][580,2225] | Install AnkiDroid, Look up Japanese anywhere, Look up Japanese anywhere. Step 1 of 3., Loo | Allow Gaten to read your AnkiDroid collection., Allow access, AnkiDroid is installed, Conn | **bounds** |
| 3 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2216] | pass / Skip / [500,2175][580,2225] | 2 / 3, A separate app from Google Play., Allow access, Connect AnkiDroid to review your de | 0, 1, 3 / 3, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity u | **bounds** |
| 4 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2216] | pass / Skip / [500,2175][580,2225] | 1, 3 / 3, Back, Checking…, Connect AnkiDroid to review your decks and save words from your | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | **bounds** |
| 5 | wait_for | `{"text": {"contains": "Recently looked up"}}` | pass / Recently looked up / [58,1424][1024,1482] | pass / Recently looked up / [55,1343][1025,1398] | Anki, Not connected, Open Anki, Daily streak. Anki study days follow your collection's rol | Anki, Permission needed, Grant access, Grant access, Permission needed | **bounds** |
| 6 | capture | `-` | pass / - / - | pass / - / - | Anki, Not connected, Open Anki, Daily streak. Anki study days follow your collection's rol | Anki, Permission needed, Grant access, Grant access, Permission needed | **text-set** |
| 7 | tap | `{"content_desc": {"exact": "Settings"}}` | pass / Settings / [58,188][1022,304] | pass / Settings / [682,2163][847,2320] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 8 | wait_for | `{"text": {"exact": "Screen lookup"}}` | pass / Screen lookup / [147,483][872,536] | pass / Screen lookup / [140,446][883,496] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 9 | capture | `-` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 10 | tap | `{"content_desc": {"contains": "Text recognition."}}` | pass / - / - | pass / Text recognition. Standard / [55,549][1025,699] | - | - | **match** |
| 11 | wait_for | `{"text": {"exact": "Standard"}}` | pass / Standard / [58,386][1022,558] | pass / Standard / [55,353][1025,516] | - | - | **bounds** |
| 12 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 13 | assert_text | `{"text": {"exact": "Standard"}}` | pass / Standard / [58,386][1022,558] | pass / Standard / [55,353][1025,516] | - | - | **bounds** |
| 14 | assert_text | `{"text": {"exact": "Enhanced"}}` | pass / Enhanced / [153,592][338,645] | pass / Enhanced / [145,549][322,599] | - | - | **bounds** |
| 15 | assert_text | `{"text": {"contains": "Better for stylised fonts"}}` | pass / Better for stylised fonts, novels and harder-to-read text / [153,655][1023,700] | pass / Better for stylised fonts, novels and harder-to-read text / [145,609][1025,652] | - | - | **bounds** |
| 16 | tap | `{"content_desc": {"contains": "Enhanced"}}` | pass / Enhanced, Recommended / [58,560][1022,732] | pass / Enhanced, Recommended / [55,519][1025,682] | Back, Better for stylised fonts, novels and harder-to-read text, Clear, regular text in st | - | **bounds** |
| 17 | wait_for | `{"text": {"exact": "Download Enhanced?"}}` | pass / Download Enhanced? / [267,930][950,988] | pass / Download Enhanced? / [90,861][990,1539] | Back, Better for stylised fonts, novels and harder-to-read text, Clear, regular text in st | - | **bounds** |
| 18 | capture | `-` | pass / - / - | pass / - / - | Back, Better for stylised fonts, novels and harder-to-read text, Clear, regular text in st | - | **text-set** |
| 19 | assert_text | `{"text": {"exact": "Download Enhanced?"}}` | pass / Download Enhanced? / [267,930][950,988] | pass / Download Enhanced? / [90,861][990,1539] | Back, Better for stylised fonts, novels and harder-to-read text, Clear, regular text in st | - | **bounds** |
| 20 | assert_text | `{"text": {"exact": "Download size"}}` | pass / Download size / [131,1049][356,1097] | pass / Download size / [150,1056][361,1101] | Back, Better for stylised fonts, novels and harder-to-read text, Clear, regular text in st | - | **bounds** |
| 21 | assert_text | `{"text": {"contains": "Reads screen and novel text better"}}` | pass / Reads screen and novel text better, then works offline. / [131,1133][950,1239] | pass / Reads screen and novel text better, then works offline. / [150,1136][930,1236] | Back, Better for stylised fonts, novels and harder-to-read text, Clear, regular text in st | - | **bounds** |
| 22 | assert_text | `{"text": {"contains": "Wi-Fi recommended"}}` | pass / Wi-Fi recommended. Mobile data may incur charges. / [131,1271][950,1316] | pass / Wi-Fi recommended. Mobile data may incur charges. / [150,1266][930,1309] | Back, Better for stylised fonts, novels and harder-to-read text, Clear, regular text in st | - | **bounds** |
| 23 | assert_text | `{"text": {"exact": "Cancel"}}` | pass / Cancel / [450,1405][579,1458] | pass / Cancel / [425,1359][608,1479] | Back, Better for stylised fonts, novels and harder-to-read text, Clear, regular text in st | - | **bounds** |
| 24 | assert_text | `{"text": {"exact": "Download"}}` | pass / Download / [695,1399][887,1452] | pass / Download / [628,1349][930,1479] | Back, Better for stylised fonts, novels and harder-to-read text, Clear, regular text in st | - | **bounds** |
| 25 | tap | `{"text": {"exact": "Cancel"}}` | pass / - / - | pass / Cancel / [425,1359][608,1479] | - | - | **match** |
| 26 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 27 | assert_not_visible | `{"text": {"exact": "Download Enhanced?"}}` | pass / - / - | pass / - / - | - | - | **match** |
| 28 | assert_visible | `{"text": {"exact": "Enhanced"}}` | pass / Enhanced / [153,592][338,645] | pass / Enhanced / [145,549][322,599] | - | - | **bounds** |
| 29 | back | `-` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 30 | wait_for | `{"text": {"exact": "Screen lookup"}}` | pass / Screen lookup / [147,483][872,536] | pass / Screen lookup / [140,446][883,496] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |
| 31 | capture | `-` | pass / - / - | pass / - / - | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **text-set** |
| 32 | assert_text | `{"text": {"exact": "Standard"}}` | pass / Standard / [808,645][951,693] | pass / Standard / [822,601][957,646] | Anki cards. Not installed, Not installed | Anki cards. Permission needed, Permission needed | **bounds** |

Counts: bounds=21, match=5, text-set=7
