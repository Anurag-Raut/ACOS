| case | quiet p50/p95 ms | quiet bound | quiet false pass | quiet early render | event p50/p95 ms | event bound | event false pass | event early render |
|---|---:|---|---:|---:|---:|---|---:|---:|
| anim | 1044.01/1060.95 | strict-quiet:10 | 0 | 0 | 931.89/933.8 | strict-event:10 | 0 | 0 |
| delay40 | 1044.84/1047.76 | strict-quiet:10 | 0 | 0 | 931.94/933.82 | strict-event:10 | 0 | 0 |
| delay200 | 1046.33/1055.67 | strict-quiet:10 | 0 | 0 | 931.35/932.11 | strict-event:10 | 0 | 0 |
| delay1000 | 1050.21/1217.49 | strict-quiet:10 | n/a (long delay) | n/a | 1093.07/1104.1 | strict-event:10 | n/a (long delay) | n/a |
| background | 1044.02/1054.8 | strict-quiet:10 | 0 | 0 | 931.23/932.67 | strict-event:10 | 0 | 0 |
| chain | 1044.52/1057.48 | strict-quiet:10 | 0 | 0 | 931.1/933.0 | strict-event:10 | 0 | 0 |
| dialog | 1079.51/1080.59 | strict-quiet:10 | 0 | 0 | 940.65/946.3 | strict-event:10 | 0 | 0 |
| activity | 417.51/418.33 | strict-quiet:10 | 0 | 0 | 303.85/314.75 | strict-event:10 | 0 | 0 |
| list | 1050.74/1060.17 | strict-quiet:10 | 0 | 0 | 932.14/934.93 | strict-event:10 | 0 | 0 |
| progress | 4549.83/4566.25 | timeout:5 | n/a (timeout) | n/a | 4539.92/4542.3 | timeout:5 | n/a (timeout) | n/a |
| layout | 1048.67/1064.3 | strict-quiet:10 | 0 | 0 | 930.59/931.79 | strict-event:10 | 0 | 0 |
| rapid | 1060.44/1062.91 | strict-quiet:10 | 0 | 0 | 950.81/968.07 | strict-event:5 | 0 | 0 |

| case | quiet notes | event notes |
|---|---|---|
| anim | tree_ok 10/10, png_ok 10/10 | tree_ok 10/10, png_ok 10/10 |
| delay40 | tree_ok 10/10, png_ok 10/10 | tree_ok 10/10, png_ok 10/10 |
| delay200 | tree_ok 10/10, png_ok 10/10 | tree_ok 10/10, png_ok 10/10 |
| delay1000 | early replies 0/10, snapshot before final 0/10, final reached 1/10 | early replies 0/10, snapshot before final 0/10, final reached 1/10 |
| background | tree_ok 10/10, png_ok 10/10 | tree_ok 10/10, png_ok 10/10 |
| chain | tree_ok 10/10, png_ok 10/10 | tree_ok 10/10, png_ok 10/10 |
| dialog | tree_ok 10/10, png_ok 10/10 | tree_ok 10/10, png_ok 10/10 |
| activity | tree_ok 10/10, png_ok 10/10 | tree_ok 10/10, png_ok 10/10 |
| list | tree_ok 10/10, png_ok 10/10 | tree_ok 10/10, png_ok 10/10 |
| progress | timeouts 5/5, never settled 5/5 | timeouts 5/5, never settled 5/5 |
| layout | tree_ok 10/10, png_ok 10/10 | tree_ok 10/10, png_ok 10/10 |
| rapid | tree_ok 10/10, png_ok 10/10 | tree_ok 5/5, png_ok 5/5 |

| counter M8 | p50 ms | p95 ms | ok/n | bounds |
|---|---:|---:|---:|---|
| quiet | 1073.57 | 1087.09 | 20/20 | strict-quiet:20 |
| event | 955.18 | 968.45 | 20/20 | strict-event:20 |
