## Linear model of settled tree PSS: T(N) = pool + slope*N (MiB)

| arm | fit points | pool MiB | slope MiB/session | residual per N (obs-fit) |
|---|---|---:|---:|---|
| exec | N=[5, 10, 20] | 190 | 37.2 | N=1: +6.4, N=2: -45.2, N=5: +0.7, N=10: -1.0, N=20: +0.3 |
| zygote-on | N=[10, 20] | 161 | 24.2 | N=10: -0.0, N=20: +0.0 |

## Per-class private MiB per session (unique to the session, immune to foreign-sharer dilution)

| class | exec N=20 | zygote N=20 |
|---|---:|---:|
| boot-image | 10.18 | 0.03 |
| anon-other | 9.76 | 10.19 |
| dalvik-other | 7.08 | 6.36 |
| shared-lib | 4.46 | 3.78 |
| odex-vdex-oat | 3.80 | 0.01 |
| other-file | 0.80 | 0.88 |
| dalvik-heap | 0.54 | 0.57 |
| hwui-skia | 0.19 | 0.21 |
| binder | 0.11 | 0.12 |
| gl | 0.05 | 0.06 |
| framework-dex | 0.02 | 0.41 |
| fonts | 0.01 | 0.00 |
| apk-dex | 0.01 | 0.58 |
| **sum** | 37.0 | 23.2 |
