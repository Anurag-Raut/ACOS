# M1 startup: 01_hello on the real core library

N = 20 runs per mode, release build (`target/release/acos`), same host, no emulator.

| stage | real p50 | real p95 | bootstrap p50 | bootstrap p95 |
|---|---:|---:|---:|---:|
| total wall (CLI) | 25 | 26 | 0 | 0 |
| boot-classpath open + VM bring-up (`Vm::new`) | 16 | 17 | 0 | 0 |
| first user bytecode -> exit | 8 | 9 | 0 | 0 |
| peak RSS (KiB, VmHWM) | 36016 | 36100 | 5024 | 5080 |

| comparison | wall p50 (ms) | ratio vs ART |
|---|---:|---:|
| real Android 14 ART (`dalvikvm64` hello, reference emulator) | 71 | 1.00x |
| ACOS `--corelib=real` (this host) | 25 | 0.35x |
| ACOS `--corelib=bootstrap` (this host) | 0 | 0.00x |

Notes: the ART number is a cold `dalvikvm64` hello on an x86_64 KVM emulator with a pre-initialised boot image. ACOS has no boot image and runs `<clinit>` from scratch, so the real-corelib number includes class loading and static initialisation that ART amortises into the image.

