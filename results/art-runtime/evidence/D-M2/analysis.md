# D-M2a measured mapping breakdown -- 2026-11-12T07:29:49+00:00

## Runs

| run | sessions | total anon+shmem MiB | incremental MiB/session | cgroup anon MiB | cgroup shmem MiB | survivors |
|---|---|---|---|---|---|---|
| n1-rep0 | 1 | 117.555 | 116.094 | 113.371 | 18.281 | 0 |
| n10-rep0 | 10 | 1142.547 | 114.109 | 974.21 | 182.809 | 0 |
| n10-rep1 | 10 | 1148.829 | 114.736 | 980.582 | 182.809 | 0 |
| n5-rep0 | 5 | 574.96 | 114.699 | 497.879 | 91.405 | 0 |
| n5-rep1 | 5 | 570.054 | 113.719 | 492.988 | 91.405 | 0 |

## N=1: top mapping groups by Private_Dirty

Per-session = sum over the 1 sessions / 1; unique = sum(Pss) across sessions (each shared page counted once); shared = sum(Rss) - sum(Pss).

| group | Rss/session | Pss/session | Private_Dirty/session | Private_Clean/session | Shared_Clean/session | Shared_Dirty/session | unique MiB | shared MiB |
|---|---|---|---|---|---|---|---|---|
| dalvik heap: main/region space | 40.23 | 40.23 | 40.23 | 0.00 | 0.00 | 0.00 | 40.23 | 0.00 |
| boot image (boot*.art/.oat/.vdex) | 40.28 | 40.28 | 26.34 | 13.95 | 0.00 | 0.00 | 40.28 | 0.00 |
| acos-art host/compositor (anon) | 11.39 | 11.39 | 6.64 | 4.75 | 0.00 | 0.00 | 11.39 | 0.00 |
| malloc (scudo/libc_malloc/[heap]) | 5.89 | 5.89 | 5.89 | 0.00 | 0.00 | 0.00 | 5.89 | 0.00 |
| other .so | 52.08 | 42.78 | 4.43 | 35.92 | 11.65 | 0.09 | 42.78 | 9.30 |
| dalvik linear alloc + other dalvik anon | 2.30 | 2.30 | 2.30 | 0.00 | 0.00 | 0.00 | 2.30 | 0.00 |
| other | 2.03 | 1.88 | 0.93 | 0.89 | 0.14 | 0.06 | 1.88 | 0.15 |
| art runtime libs | 10.79 | 10.77 | 0.35 | 10.39 | 0.06 | 0.00 | 10.77 | 0.03 |
| stacks | 0.35 | 0.30 | 0.28 | 0.00 | 0.04 | 0.04 | 0.30 | 0.06 |
| dalvik heap: large object space | 0.13 | 0.13 | 0.13 | 0.00 | 0.00 | 0.00 | 0.13 | 0.00 |
| other memfd/ashmem | 0.14 | 0.11 | 0.04 | 0.04 | 0.00 | 0.06 | 0.11 | 0.03 |
| dalvik heap: non-moving space | 0.02 | 0.02 | 0.02 | 0.00 | 0.00 | 0.00 | 0.02 | 0.00 |
| dalvik heap: zygote space | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| dalvik jit code cache | 0.02 | 0.01 | 0.00 | 0.00 | 0.00 | 0.02 | 0.01 | 0.01 |
| fonts (.ttf/.otf/.ttc) | 15.60 | 15.60 | 0.00 | 15.60 | 0.00 | 0.00 | 15.60 | 0.00 |
| icu (.dat) | 0.75 | 0.50 | 0.00 | 0.26 | 0.48 | 0.00 | 0.50 | 0.24 |
| resources (apk) | 6.25 | 6.25 | 0.00 | 6.25 | 0.00 | 0.00 | 6.25 | 0.00 |
| framework jar/dex | 36.13 | 36.00 | 0.00 | 35.87 | 0.27 | 0.00 | 36.00 | 0.13 |

* session 0: process anon+shmem 87.85 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0

## N=5: top mapping groups by Private_Dirty

Per-session = sum over the 5 sessions / 5; unique = sum(Pss) across sessions (each shared page counted once); shared = sum(Rss) - sum(Pss).

| group | Rss/session | Pss/session | Private_Dirty/session | Private_Clean/session | Shared_Clean/session | Shared_Dirty/session | unique MiB | shared MiB |
|---|---|---|---|---|---|---|---|---|
| dalvik heap: main/region space | 39.15 | 39.15 | 39.15 | 0.00 | 0.00 | 0.00 | 195.73 | 0.00 |
| boot image (boot*.art/.oat/.vdex) | 40.40 | 29.27 | 26.34 | 0.06 | 13.99 | 0.00 | 146.34 | 55.63 |
| acos-art host/compositor (anon) | 11.52 | 7.73 | 6.75 | 0.02 | 4.75 | 0.00 | 38.66 | 18.96 |
| malloc (scudo/libc_malloc/[heap]) | 5.83 | 5.83 | 5.83 | 0.00 | 0.00 | 0.00 | 29.17 | 0.00 |
| other .so | 52.38 | 12.38 | 4.43 | 0.14 | 47.73 | 0.09 | 61.88 | 200.00 |
| dalvik linear alloc + other dalvik anon | 2.35 | 2.35 | 2.35 | 0.00 | 0.00 | 0.00 | 11.74 | 0.00 |
| other | 2.04 | 1.17 | 0.93 | 0.01 | 1.04 | 0.06 | 5.86 | 4.36 |
| art runtime libs | 10.83 | 2.43 | 0.35 | 0.00 | 10.48 | 0.00 | 12.13 | 42.02 |
| stacks | 0.35 | 0.30 | 0.28 | 0.00 | 0.04 | 0.04 | 1.48 | 0.27 |
| dalvik heap: large object space | 0.08 | 0.08 | 0.08 | 0.00 | 0.00 | 0.00 | 0.41 | 0.00 |
| other memfd/ashmem | 0.14 | 0.08 | 0.04 | 0.00 | 0.04 | 0.06 | 0.41 | 0.32 |
| dalvik heap: non-moving space | 0.02 | 0.02 | 0.02 | 0.00 | 0.00 | 0.00 | 0.10 | 0.00 |
| dalvik heap: zygote space | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.02 | 0.00 |
| dalvik jit code cache | 0.02 | 0.01 | 0.00 | 0.00 | 0.00 | 0.02 | 0.06 | 0.06 |
| fonts (.ttf/.otf/.ttc) | 15.45 | 3.30 | 0.00 | 0.14 | 15.31 | 0.00 | 16.48 | 60.79 |
| icu (.dat) | 0.75 | 0.10 | 0.00 | 0.00 | 0.75 | 0.00 | 0.50 | 3.23 |
| resources (apk) | 6.25 | 1.25 | 0.00 | 0.00 | 6.25 | 0.00 | 6.24 | 25.02 |
| framework jar/dex | 36.11 | 7.29 | 0.00 | 0.07 | 36.05 | 0.00 | 36.45 | 144.11 |

* session 0: process anon+shmem 84.65 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0
* session 1: process anon+shmem 85.31 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0
* session 2: process anon+shmem 88.21 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0
* session 3: process anon+shmem 87.77 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0
* session 4: process anon+shmem 88.17 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0

## N=10: top mapping groups by Private_Dirty

Per-session = sum over the 10 sessions / 10; unique = sum(Pss) across sessions (each shared page counted once); shared = sum(Rss) - sum(Pss).

| group | Rss/session | Pss/session | Private_Dirty/session | Private_Clean/session | Shared_Clean/session | Shared_Dirty/session | unique MiB | shared MiB |
|---|---|---|---|---|---|---|---|---|
| dalvik heap: main/region space | 38.85 | 38.85 | 38.85 | 0.00 | 0.00 | 0.00 | 388.49 | 0.00 |
| boot image (boot*.art/.oat/.vdex) | 40.42 | 27.80 | 26.34 | 0.01 | 14.07 | 0.00 | 277.98 | 126.26 |
| acos-art host/compositor (anon) | 11.68 | 7.36 | 6.87 | 0.00 | 4.80 | 0.00 | 73.62 | 43.13 |
| malloc (scudo/libc_malloc/[heap]) | 5.86 | 5.86 | 5.86 | 0.00 | 0.00 | 0.00 | 58.62 | 0.00 |
| other .so | 52.27 | 8.37 | 4.43 | 0.04 | 47.72 | 0.09 | 83.67 | 438.98 |
| dalvik linear alloc + other dalvik anon | 2.37 | 2.37 | 2.37 | 0.00 | 0.00 | 0.00 | 23.72 | 0.00 |
| other | 2.01 | 1.06 | 0.93 | 0.00 | 1.02 | 0.06 | 10.64 | 9.48 |
| stacks | 0.55 | 0.49 | 0.47 | 0.00 | 0.04 | 0.04 | 4.91 | 0.55 |
| art runtime libs | 10.81 | 1.38 | 0.35 | 0.00 | 10.47 | 0.00 | 13.79 | 94.35 |
| dalvik heap: large object space | 0.06 | 0.06 | 0.06 | 0.00 | 0.00 | 0.00 | 0.58 | 0.00 |
| other memfd/ashmem | 0.14 | 0.08 | 0.04 | 0.00 | 0.04 | 0.06 | 0.77 | 0.67 |
| dalvik heap: non-moving space | 0.02 | 0.02 | 0.02 | 0.00 | 0.00 | 0.00 | 0.20 | 0.00 |
| dalvik heap: zygote space | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.04 | 0.00 |
| dalvik jit code cache | 0.02 | 0.01 | 0.00 | 0.00 | 0.00 | 0.02 | 0.12 | 0.12 |
| fonts (.ttf/.otf/.ttc) | 15.49 | 1.62 | 0.00 | 0.04 | 15.45 | 0.00 | 16.19 | 138.76 |
| icu (.dat) | 0.75 | 0.05 | 0.00 | 0.00 | 0.75 | 0.00 | 0.49 | 6.97 |
| resources (apk) | 6.25 | 0.62 | 0.00 | 0.00 | 6.25 | 0.00 | 6.23 | 56.27 |
| framework jar/dex | 36.05 | 3.64 | 0.00 | 0.01 | 36.03 | 0.00 | 36.44 | 324.06 |

* session 0: process anon+shmem 84.11 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0
* session 1: process anon+shmem 88.50 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0
* session 2: process anon+shmem 87.49 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0
* session 3: process anon+shmem 87.93 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0
* session 4: process anon+shmem 85.35 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0
* session 5: process anon+shmem 84.66 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0
* session 6: process anon+shmem 83.90 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0
* session 7: process anon+shmem 88.76 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0
* session 8: process anon+shmem 89.85 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0
* session 9: process anon+shmem 87.91 MiB; gralloc memfds: 2 unique, 18.20 MiB, 7 fds, owners [['guest', 'launcher'], ['guest', 'launcher']]; mapped at settle: 0

## Marginal per session, per group (MiB/session)

| group | (N5-N1)/4 Pss | (N5-N1)/4 Rss | (N5-N1)/4 Private_Dirty | (N10-N5)/5 Pss | (N10-N5)/5 Rss |
|---|---|---|---|---|---|
| dalvik heap: main/region space | 38.88 | 38.88 | 38.88 | 38.55 | 38.55 |
| boot image (boot*.art/.oat/.vdex) | 26.51 | 40.42 | 26.34 | 26.33 | 40.45 |
| acos-art host/compositor (anon) | 6.82 | 11.56 | 6.77 | 6.99 | 11.83 |
| malloc (scudo/libc_malloc/[heap]) | 5.82 | 5.82 | 5.82 | 5.89 | 5.89 |
| other .so | 4.78 | 52.45 | 4.43 | 4.36 | 52.15 |
| dalvik linear alloc + other dalvik anon | 2.36 | 2.36 | 2.36 | 2.40 | 2.40 |
| other | 0.99 | 2.05 | 0.93 | 0.96 | 1.98 |
| art runtime libs | 0.34 | 10.84 | 0.35 | 0.33 | 10.80 |
| stacks | 0.30 | 0.35 | 0.28 | 0.69 | 0.74 |
| fonts (.ttf/.otf/.ttc) | 0.22 | 15.42 | 0.00 | -0.06 | 15.54 |
| framework jar/dex | 0.11 | 36.11 | 0.00 | -0.00 | 35.99 |
| other memfd/ashmem | 0.07 | 0.14 | 0.04 | 0.07 | 0.14 |
| dalvik heap: large object space | 0.07 | 0.07 | 0.07 | 0.03 | 0.03 |
| dalvik heap: non-moving space | 0.02 | 0.02 | 0.02 | 0.02 | 0.02 |
| dalvik jit code cache | 0.01 | 0.02 | 0.00 | 0.01 | 0.02 |
| dalvik heap: zygote space | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| icu (.dat) | -0.00 | 0.75 | 0.00 | -0.00 | 0.75 |
| resources (apk) | -0.00 | 6.25 | 0.00 | -0.00 | 6.25 |

