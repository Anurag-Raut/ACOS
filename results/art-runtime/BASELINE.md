# ART memory baseline with real framework classes loaded

Owner: orchestrated measurement, 2026-11-11. Scope: measurement only. No ACOS source edited, nothing committed. Scratch was in `.cache/art-base/` and was deleted at the end.

## 0. Answer

- **Real framework classes on ART cost about 16–21 MiB of private memory per session with the imageless (cold) path, and about 64–66 MiB per session with the AOT boot image.**
- **The shared, file-backed part is fixed: about 100–113 MiB (cold) and about 60–83 MiB (AOT) for the whole host, no matter how many sessions run.**
- The rung contents (Parcel, Looper, Resources) add almost nothing on top of bare hello. The variant (cold vs AOT) is what matters.

## 1. What was measured

| item | how |
|---|---|
| ART runtime | `dalvikvm64` from the product sysroot `.cache/art/sysroot` (Android 14, 35 boot jars, device `BOOTCLASSPATH`), the same sysroot `acos-art` verifies 27/27 on |
| rungs | **hello**: `01_hello` `Main.main` (E1A bare). **parcel+looper**: E2D `stepParcel` + `stepLooper`. **resources**: E2D `stepResources` (AssetManager + `hello-views.apk` + `Resources`) |
| variants | **cold** = imageless JIT (product default). **aot** = `-Ximage:/data/img/boot.art` built by the image's own `dex2oat64` (3.7 s, 140 MB) |
| N | 1 (3 reps), 5 (1 rep), 10 (1 rep). N independent sessions, each its own process behind its own `bwrap` |
| preload | `hello`: `libartshim.so` (product shim). Framework rungs: `libartshim.so` then `libbindershim.so` (E2D, binder stand-in on) |
| sampling | each process runs the rung, prints READY, then idles on stdin. After all N are READY and 3 s idle, every process tree (bwrap + dalvikvm) is read from `/proc/<pid>/smaps_rollup` |
| PSS | sum of `Pss` over all processes in the N sessions. Each session's `bwrap` monitor (≈0.2 MiB PSS) is counted once per session |
| per-proc marginal | `(sum PSS at N − sum PSS at N=1)/(N−1)`. N=1 row shows the full single-session cost |
| startup | wall ms from `Popen` to READY, so it includes `bwrap` setup, ART boot and the rung's own work. N=1 is the median of 3 reps; N≥5 is the median across the concurrent processes |

Correctness: each rung was also run through `research/art-feasibility/d-framework/scripts/run.sh parcel|looper|resources` (E1A sysroot). All PASS, `failures=0`, rc=0. The harness holder exists because `Main.main` ends in `System.exit`, so `run.sh` cannot idle a process for sampling. The holder calls the same `stepX` methods. Inside the harness, the three framework rungs also reported all PASS lines and zero FAIL lines in every process.

Host RAM: about 58% used (35.6 of 61.9 GB) at checks, below the 90% ceiling. Each harness run was bounded by `tools/guarded.sh --mem 12G`.

## 2. Table: rung | N | sum PSS | per-proc marginal | private dirty | startup

Sum PSS, marginal, and private dirty are MiB. Startup is ms (median, with min–max range in the second table). Private dirty is the sum over the N sessions.

| rung | variant | N | sum PSS MiB | per-proc marginal MiB | private dirty MiB | startup ms |
|---|---|---:|---:|---:|---:|---|
| hello | cold | 1 | 118.6 | 118.6 | 15.8 | 162 |
| hello | cold | 5 | 185.5 | 16.7 | 78.9 | 169 |
| hello | cold | 10 | 264.9 | 16.2 | 157.8 | 180 |
| hello | aot | 1 | 123.4 | 123.4 | 64.0 | 76 |
| hello | aot | 5 | 381.9 | 64.6 | 316.6 | 98 |
| hello | aot | 10 | 699.7 | 64.0 | 633.1 | 129 |
| parcel+looper | cold | 1 | 120.4 | 120.4 | 17.0 | 173 |
| parcel+looper | cold | 5 | 188.6 | 17.0 | 81.7 | 179 |
| parcel+looper | cold | 10 | 270.4 | 16.7 | 163.1 | 187 |
| parcel+looper | aot | 1 | 131.1 | 131.1 | 63.5 | 81 |
| parcel+looper | aot | 5 | 394.7 | 65.9 | 319.6 | 103 |
| parcel+looper | aot | 10 | 710.6 | 64.4 | 635.2 | 132 |
| resources | cold | 1 | 132.9 | 132.9 | 21.0 | 203 |
| resources | cold | 5 | 222.2 | 22.3 | 106.6 | 210 |
| resources | cold | 10 | 325.2 | 21.4 | 208.9 | 224 |
| resources | aot | 1 | 147.1 | 147.1 | 64.5 | 87 |
| resources | aot | 5 | 412.6 | 66.4 | 322.4 | 108 |
| resources | aot | 10 | 736.6 | 65.5 | 644.8 | 139 |

Detail (means over reps): Pss_Anon and Pss_File are the sums across the N sessions.

| rung | variant | N | Pss_Anon MiB | Pss_File MiB | sum PSS range over reps | startup min–max ms |
|---|---|---:|---:|---:|---|---|
| hello | cold | 1 | 15.8 | 102.9 | 118.3–118.8 (3) | 162–168 |
| hello | cold | 5 | 78.8 | 106.6 | – | 169–169 |
| hello | cold | 10 | 157.6 | 107.1 | 192.2 (first pass) vs 264.9 (this pass), see §4 | 180–181 |
| hello | aot | 1 | 64.0 | 59.4 | 121.9–124.9 (3) | 76–76 |
| hello | aot | 5 | 316.5 | 65.4 | – | 88–108 |
| hello | aot | 10 | 633.0 | 66.6 | 641.5 (first pass) vs 699.7 (this pass) | 96–151 |
| parcel+looper | cold | 1 | 16.9 | 103.5 | 119.7–121.6 (3) | 173–173 |
| parcel+looper | cold | 5 | 81.6 | 106.9 | – | 178–179 |
| parcel+looper | cold | 10 | 163.0 | 107.2 | – | 186–192 |
| parcel+looper | aot | 1 | 63.5 | 67.6 | 130.9–131.3 (3) | 81–81 |
| parcel+looper | aot | 5 | 319.6 | 75.1 | – | 93–113 |
| parcel+looper | aot | 10 | 635.0 | 75.5 | – | 99–154 |
| resources | cold | 1 | 21.0 | 111.9 | 132.8–133.1 (3) | 198–203 |
| resources | cold | 5 | 106.6 | 115.4 | – | 209–210 |
| resources | cold | 10 | 208.9 | 116.0 | 325.2 (both passes) | 223–224 |
| resources | aot | 1 | 64.5 | 82.6 | 146.9–147.4 (3) | 86–87 |
| resources | aot | 5 | 322.3 | 90.3 | – | 98–118 |
| resources | aot | 10 | 644.6 | 91.8 | 736.6 | 110–166 |

Two-parameter fit over N=1, 5, 10 (sum PSS = fixed + N × per-session):

| rung | variant | fixed MiB | per-session MiB |
|---|---|---:|---:|
| hello | cold | 103.1 | 16.2 |
| hello | aot | 60.2 | 64.0 |
| parcel+looper | cold | 104.3 | 16.6 |
| parcel+looper | aot | 69.0 | 64.3 |
| resources | cold | 113.0 | 21.3 |
| resources | aot | 83.0 | 65.5 |

## 3. What dominates, per mapping (one idle process, probed separately)

Cold (hello, one session, PSS MiB / private dirty MiB): `framework.jar` 38.1 / 0.0, `libart.so` 6.0 / 0.1, `core-oj.jar` 5.3 / 0.0, `telephony-common.jar` 3.7 / 0.0, `core-icu4j.jar` 2.8 / 0.0, `libcrypto.so` 2.2 / 0.1. Private anon (`dalvik-main`, `dalvik-card`, `dalvik-non-moving`, scudo) is about 2 MiB each, about 16 MiB in total. Jars and libs are shared clean pages.

AOT (hello, one session, PSS MiB / private dirty MiB): boot image `boot-framework.art` 30.5 / 30.5, `boot.art` 4.4 / 4.4, `boot-telephony-common.art` 2.7 / 2.7, `boot-core-icu4j.art` 1.7 / 1.7, `boot-apache-xml.art` 1.5 / 1.5. The image mappings are almost entirely private-dirty per process, about 48 MiB across the image files. The jars and libs are still shared clean. The probe did not isolate whether this comes from relocation or from runtime writes into image objects.

## 4. Caveats

- N=5 and N=10 are one rep each. N=10 cold hello gave 192.2 MiB in the first full pass and 264.9 MiB in the second. The first-pass anon breakdown was not kept, so the spread is not explained. Treat cold N=10 hello as ±35%. Other N=10 rows were within about 9% between passes (AOT hello 641.5 vs 699.7; resources cold 325.2 in both).
- Sampling is at idle, 3 s after the last process reached READY. It is not a peak. ART's JIT and GC may still move during a real session.
- The startup numbers include `bwrap` setup and the rung's own work. They are not a bare `dalvikvm` boot time. The AOT cold-start drop (162 → 76 ms) matches the 76 ms in `results/art-runtime/F.md`.
- The sandbox preloads `libartshim.so` (the product shim). It makes ART's boot-time CHECK ("No mainline framework library found") non-fatal, and it is not in the E2D run.
- The framework rungs' PSS includes the `libbindershim.so` binder stand-in (memfd, no `/dev/binder`), which E2D used.

## 5. Comparison with the Rust VM (`results/art-feasibility/E3-M9-peak-pss.md`)

Comparable: both are **one OS process per session** (E3's M9 one-shot / kept-daemon figure). E3's M2 figure is an in-process amortised number and is not comparable.

Not comparable in workload: M9 runs a full ACOS session (`observe --apk counter --tree`) with the Rust VM, font loading, a 1080×2400 frame surface, and the app APK. This ART baseline runs framework-class slices with no UI, no surfaces and no rendering. So this is an architecture comparison, not a like-for-like app comparison.

| engine | N | sum / per-session PSS MiB | private dirty / anon per session MiB |
|---|---:|---|---|
| ACOS Rust VM, one process per session (M9 kept daemons) | 1 | 595.5 (launch peak) / 594.7 (idle) | 562.4 |
| ACOS Rust VM, M9 one-shot | 5 | 532.5 | 524.1 |
| ACOS Rust VM, M9 kept | 20 | 527.1 | 524.3 |
| ART hello, cold | 1 / 5 / 10 | 118.6 / 185.5 / 264.9 | 15.8 / ~15.8 per session |
| ART resources, AOT | 1 / 5 / 10 | 147.1 / 412.6 / 736.6 | 64.5 / ~64.5 per session |

Per session, the ACOS engine's private memory is about 525 MiB. The ART engine is about 16–21 MiB cold (roughly 25–33× less) and about 64–66 MiB with the AOT image (roughly 8× less), on a lighter workload. E3 §4 estimated ART app processes at ~40–100 MiB PSS, and the measured ART slices are at the low end of that range.

## 6. Reproduce

Scratch harness (`.cache/art-base/`, deleted with the rest of the scratch):

1. Build the scratch data dir: copy `01_hello.dex`, the E2D `classes.dex`, `libbindershim.so`, `hello-views.apk`, the product `apex-info-list.xml`, `acos.properties`, `linkerconfig/`, and `shim/libartshim.so` into `data/`. Compile a holder class (`Hold.java`, which calls the given static methods then blocks on stdin) to `data/dex/hold/classes.dex`.
2. Build the AOT image: `target/debug/acos-art build-image --out <data>/img`.
3. Launch via a bwrap root equal to `artroot.sh` but with a read-only `/system` (no writes into the sysroot), `BOOTCLASSPATH` from `<sysroot>/BOOTCLASSPATH`, `LD_PRELOAD` as above, and `E2D_BINDER_FAKE=1` in the sandbox env (required, or the framework rungs abort in `ProcessState`).
4. Sample `smaps_rollup` for each session's process tree at idle, aggregate, and repeat for N=1, 5, 10 and both variants.

Correctness check: `research/art-feasibility/d-framework/scripts/run.sh parcel`, `looper`, `resources` (E1A sysroot; `data/apk/` needed `hello-views.apk`, which I copied in).

---

**Fixed vs per-session memory:** fixed (shared file pages, counted once) is about 100–113 MiB cold and about 60–83 MiB AOT; per session it is about 16–21 MiB cold and about 64–66 MiB AOT (N=1 to N=10, fit over three points).
**What dominates:** cold, the shared jars and libs (about 100 MiB) are fixed and the per-session cost is private heap/GC/JIT (about 16–21 MiB); AOT, the private-dirty pages of the boot image (about 48 MiB across `boot-*.art`) dominate the per-session cost.
**Trade-off:** the AOT image cuts startup from about 162–203 ms to about 76–87 ms, but adds about 48 MiB of private memory per session.
