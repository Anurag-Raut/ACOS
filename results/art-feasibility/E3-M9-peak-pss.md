# E3-M9 — why ~528 MiB peak PSS per session at N=20 while M2 reports ~39 MiB

**Scope:** read-only investigation plus measurement. No ACOS source was edited and nothing was committed.
**Build:** clean detached worktree of `HEAD 018f8ce`, release `acos` binary, `CARGO_TARGET_DIR=.cache/target-m9`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_BUILD_JOBS=2`, all runs under `tools/guarded.sh`.
Fixtures and `.cache/boot/android-34` symlinked in; `ACOS_FONT_DIR=<main>/.cache/boot/android-34/fonts`;
private `ACOS_CACHE_DIR` warmed by one launch (boot-image cache 25.6 MB). Host: 32 threads, 61.9 GiB.
Lowest `MemAvailable` seen during the runs: 23.2 GB (about 62 % used). Raw samples: `.cache/m9-out/` (gitignored, not committed).
Sampler: `.cache/m9-tools/sample.py` (scratch, outside the worktree).

## 1. What each metric measures

| Metric | Definition (BENCHMARKS.md §9.7–9.9) | What it actually hides |
|---|---|---|
| **M2** | Incremental anon+shmem per session. Sessions run as N `Runtime`s **inside one process** (`SessionPool` / `sessions-shared` style). Boot DEX, parsed tables and decoded code are `Arc`-shared once, so per-session cost is a marginal ~15.8 MiB plus `M1 / N`. | `M1 = 133.9 MiB` is the one-time shared boot image. At N=5, `133.9/5 + 15.8 ≈ 42` (reported 39.4). The `1/N` term is the in-process sharing itself. It is not isolated memory, because the Binder transport is process-global (see §9.8.2 and MEMORY_PROFILE §5). |
| **M9** | Per-session peak PSS with **one OS process per session**, each `observe --apk counter --tree`. Each process pays the full boot image itself. | Nothing is amortised. PSS does divide file-backed shared pages (APK, fonts) by sharer count, which is why `Pss_File` is small. |

So the two numbers are both correct for their definitions. M2 is an in-process amortised figure. M9 is the figure for isolated sessions. At N=5 the M2 figure is dominated by the `1/N` share of the fixed 134 MiB, which is why "N=5 incremental" looks small: it hides that the marginal session in its own process costs ~530 MiB.

## 2. Measured per-session memory (separate processes, `acos serve` daemon per session, `launch --keep`)

Per-session medians, MiB. Launch peak = max over 50 ms-ish polls. After first frame = snapshot as `launch` returned. Steady idle = 10 s later.

| N | moment | Rss | Pss | Pss_Anon | Pss_File | Private_Dirty | Shared_Clean |
|---:|---|---:|---:|---:|---:|---:|---:|
| 1 | launch peak | 771.7 | 595.5 | 562.4 | 33.1 | 562.4 | 195.2 |
| 1 | after first frame | 771.7 | 595.5 | 562.4 | 33.1 | 562.4 | 195.2 |
| 1 | idle +10 s | 770.9 | 594.7 | 562.4 | 32.4 | 562.4 | 195.2 |
| 5 | launch peak | 733.7 | 533.2 | 524.3 | 8.9 | 524.3 | 209.4 |
| 5 | idle +10 s | 733.7 | 533.2 | 524.3 | 8.9 | 524.3 | 209.4 |
| 20 | launch peak (run 1) | 733.9 | **527.1** [527.0–527.2] | 524.3 | 2.9 | 524.3 | 209.5 |
| 20 | idle +10 s (run 1) | 733.9 | 527.1 | 524.3 | 2.8 | 524.3 | 209.5 |
| 20 | launch peak (run 2, rollup-only) | 772.0 | 565.1 | 562.3 | 2.8 | 562.3 | — |

Other numbers:
- **M9-exact one-shot children** (`observe --apk counter --tree`): N=5 Pss 532.5 / Pss_Anon 524.1; N=20 Pss 507.5 (only 6 polls in 2.2 s, under-sampled, so treat as a lower bound). The earlier V1 figure of 528 at N=20 is reproduced by the kept-daemon run 1.
- All 20/20 (and 5/5, 1/1) sessions reached `first_frame`.
- **Run-to-run spread is about 7 % at N=20 (527 vs 565) and about 38 MiB at N=5 (533 vs 571 for the same control).** The control repeated three times gave 532.9 / 534.1 / 532.5. I treat ±40 MiB as host-load noise.
- **Sampling caveat:** full smaps polling gave 50 ms at N=1 but only about 300 ms at N=20 (Python parse). Rollup-only polling gives ~140 ms at N=20. Transients shorter than that can be missed.

### Category breakdown, steady idle, N=20 (median per session, Rss MiB; Pss in brackets)

| Category | Rss | Pss | Notes |
|---|---:|---:|---|
| glibc 64 MiB-aligned arena heaps (7 mappings) | 355.9 | 355.9 | private anon, live + high-water |
| anon mmap, unnamed (15 mappings) | 129.1 | 129.1 | includes large Vec / buffer allocations |
| `[heap]` brk | 38.2 | 38.2 | private anon |
| APK file maps (`framework-res.apk` and app APK, `r--s`) | 125.1 | 1.2 | already **shared** page cache |
| font file maps (Noto CJK `.ttc`, Roboto, VF `.ttf`) | 67.4 | 0.8 | already **shared** (the 41 MiB I first labelled "boot classpath" is these CJK `.ttc` files; my classifier matched `/boot` before fonts) |
| acos binary text | 14.7 | 1.5 | shared |
| libs (`libc`, `libm`) | 3.2 | 0.1 | shared |
| thread stacks | 0.6 | 0.6 | negligible |
| boot-cache **file** maps | 0 | 0 | none: the boot DEX and boot-cache payload are **read** into anon, not mapped |

Census (`ACOS_PROFILE=1`, one launch) names the live anon buckets:

| Bucket | MiB | Per-process? | Could it be shared? |
|---|---:|---|---|
| `dex_raw` (41 boot DEX, `Arc<[u8]>`, read with `fs::read`) | 63–66 | yes (anon, duplicated per process) | **yes**: map the boot DEX read-only, as APK and fonts already are |
| `dex_parsed_tables` | 22.9 | yes | derived; could be a zero-copy or mapped index |
| decoded `MethodCode` (`vm_code_bytes`) | 32.5–37.5 | yes | derived; lazy decode would cut it |
| `dex_decoded_strings` | 15.4 | yes | derived; could be lazy |
| `vm_class_meta` / `vm_heap_slots` / `vm_dex_handle_caches` / method, type, interner | 16.6 / 15.5 / 15.6 / ~5 | yes (live VM state) | no (per-session) |
| **named total** | **~190** | | |
| unnamed anon (arenas + brk + anon mmap, minus named) | ~330 | yes | see §3 |

Cumulative allocation per launch (census `allocations[]`, not live): `boot_cache_file_read` 51.1 MB over two reads, `boot_cache_payload` 51.1 MB, `boot_image_serialized` 44.4 MB, `boot_payload_snapshot` 6.8 MB, `vm_install_boot_image` 3.5 MB. These are copies made on the restore path, and they explain part of the unnamed anon.

## 3. Answers to the four questions

### (1) Why 39 vs 528

Covered in §1. The short version: M2 spreads one shared 134 MiB boot image over N sessions in one address space. M9 gives each session its own process, so each one holds its own copy of the ~525 MiB anon footprint. Nothing here is double-counted: PSS already divides shared file pages, and the anon pages really are private.

### (2) Which anon is transient, live, or shareable

- **Shareable if mapped instead of copied:** `dex_raw` ≈ 63–66 MiB (boot DEX). Boot-cache payload ≈ 25.6 MB is only shareable if the restore can borrow from it; today it is decoded into owned structures, so mapping it alone would not save much.
- **Live, per-session:** named VM state ~55 MiB (classes, heap slots, handle caches, interner), decoded code ~35 MiB, parsed tables ~23 MiB, decoded strings ~15 MiB. Total named live ≈ 190 MiB.
- **Transient or retained high-water:** most of the unnamed ~330 MiB. Evidence:
  - A **44 MiB Pss dip** at ~1.0 s during framework boot (413 → 369 MiB) shows boot-time buffers really are freed back to the kernel (mmap'd chunks).
  - **Fixed mmap threshold** (below) removes ~52 MiB of retained high-water. That is allocations between 128 KiB and ~32 MiB freed inside arenas, which glibc's dynamic threshold moves from `mmap` to the heap.
  - Restore-path copies (51 MB file read, 44 MB serialized image, 3.5 MB snapshot copy) are freed after use but stay resident in arenas.
- **Still unattributed:** roughly 280 MiB of unnamed anon after the transient and named buckets. Likely host-side state (`ServiceHost`, ATMS, Binder, FrameStore surfaces at ~9.9 MiB per 1080×2400 RGBA buffer, and allocator slack). Not measured here. Closing this needs `mallinfo2` in-use vs free, or a per-allocation tag in the census, which I did not add.

### (3) Does RSS fall after the launch peak?

**Mostly no.** Across N=20 the Pss series rises monotonically. The one exception is the boot-time dip (44 MiB, ~1 s). After the first frame, Pss is flat to within 1 MiB for at least 10 s. glibc keeps its high-water arenas. Two levers for this:
- `MALLOC_TRIM_THRESHOLD_=0` saves ~37 MiB but costs **2.25× launch wall** (W183-A, reproduced in §9.9.2). Rejected.
- `MALLOC_ARENA_MAX=1` saves ~7 MiB at no measurable wall cost (W183-A).

I could not call `malloc_trim(0)` inside a running daemon: `kernel.yama.ptrace_scope=1` blocks `gdb -p` on a non-descendant. This is the next experiment if the answer matters.

### (4) Ranked fixes

Measured where marked (M); the rest are estimates (E). Savings are per session at N=20 unless noted.

| # | Fix | Est. MiB saved / session | Evidence | Risk | Blocker today |
|---:|---|---:|---|---|---|
| 1 | **Zygote-style fork of a warm, single-threaded template** (children share boot DEX, parsed tables, decoded code, CoW) | **~380–450 (E)** | Structure already measured in MEMORY_PROFILE §5 | High | `#![forbid(unsafe_code)]` in `acos-runtime` and `acos-vm`. The only fork is `acos-sandbox/src/live.rs:41`, which is allowed unsafe. Forking must happen before worker threads exist. Pid, RNG, clock, binder identity and fds must be re-seeded. Layout is **not** a blocker: the heap is `Vec`-based and fork preserves addresses; only written pages are duplicated. Arc refcount writes dirty one header page, not the DEX bytes. |
| 2 | **Map the boot DEX read-only** (MAP_PRIVATE / shared page cache) instead of `fs::read` | **~60 at N≥5 (E)**, 0 at N=1 | `dex_raw` 63–66 MiB anon (census). APK and fonts already mapped `r--s` at ~1 MiB PSS each | Medium | `forbid(unsafe_code)` in `acos-runtime`. `memmap2::Mmap::map` is `unsafe`, already used in `acos-render/src/text/fonts.rs:270` and `acos-framework-natives`. Mapping needs to move to an unsafe-allowed crate. Atomic rename-publish means the mapped inode stays valid. |
| 3 | **Pin glibc mmap threshold** (`MALLOC_MMAP_THRESHOLD_` set by the launcher, or `mallopt`) | **~52 (M)** at 1 MiB; **~54 (M)** at 128 KiB; ~15 (M, single run) at 16 MiB | N=5, 3 reps each: control 532.9 / 534.1 / 532.5; 128 KiB 479.1 / 479.1 / 479.1; 1 MiB 481.6 / 481.1 / 481.0 | Low (perf only) | **Costs launch wall**: control 1.39–1.42 s; 1 MiB 2.63–2.66 s (+90 %); 128 KiB 2.93–3.00 s (+110 %). 16 MiB: 1.54 s (+9 %). Pick the threshold by the wall budget. |
| 4 | Stream the boot-cache restore instead of read → copy → decode (51 MB file read, 44 MB serialized image, payload copies) | ~20–25 (E) | census `allocations[]`; the transient is largely the same bytes as #3 | Medium | Overlaps #3. Not additive once #3 is in. |
| 5 | Lazy `dex_decoded_strings` (materialise on demand) | 10–15 (E) | 15.4 MiB census | Medium | Interner identity semantics. |
| 6 | Shrink `vm_heap_slots` after GC (`len 148k / cap 290k`) | ~7 (E) | 15.5 MiB census; W139-A proposal | Low | None known. |
| 7 | `MALLOC_ARENA_MAX=1` | ~7 (M, W183-A) | 529.9 → 522.6 MiB | Low | None; no wall cost measured. |
| 8 | Sparse / shared-prefix `vm_dex_handle_caches` | 5–10 (E) | 15.6 MiB census, W146-A already cut 13.5 | Medium | Hot dispatch path. |
| — | Drop `dex_raw` after parse | up to 63, but ~25 net (E) | raw bytes are still used by lazy method decode | High | Needs all methods pre-decoded, which grows `vm_code_bytes` by ~37 MiB. Do #2 first. |
| — | `MALLOC_TRIM_THRESHOLD_=0` | 37 (M) | W183-A | — | **2.25× launch wall. Rejected.** |

Recommended order: **#7 (free), #6, #3 with a threshold chosen against the wall budget, #2 (needs one unsafe-allowed crate), then #1** as the structural fix. #1 is the only path that gets per-session PSS near ART-scale.

## 4. Comparison with real ART and a forked warm parent (estimate, not measured)

Real ART forks each app process from a zygote that already holds the boot image. The boot image and framework code are shared and clean, so PSS per app process is dominated by app-private dirty pages. From general knowledge, not verified in this run: a simple app process on Android 14 typically shows **~40–100 MiB PSS**, of which roughly **20–50 MiB is private dirty**. The shared boot image (tens of MiB, file-backed) is counted at 1/N in PSS.

For ACOS today the comparable private part is ~525 MiB anon per process, roughly **10× the estimated ART private-dirty figure**. A zygote-style fork (fix #1) would cut the boot-derived part (DEX, parsed tables, decoded code, strings, about 135 MiB, plus the 25 MB boot payload) to shared pages. The residual private cost would then be the per-session live VM state (~55 MiB named) plus whatever pages each session touches. That suggests roughly **60–150 MiB PSS per session** after fix #1, which is in the ART range but still above it. The gap that remains is mostly live VM state and unnamed host-side allocations (§3). This is an estimate, not a measurement.

## 5. Reproduce

```sh
# worktree (cleaned up after this run)
git worktree add --detach .cache/wt-m9 HEAD
# link fixtures and boot cache, then:
CARGO_TARGET_DIR=.cache/target-m9 CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=2 \
  tools/guarded.sh --mem 12G --time 3600 -- cargo build --release -p acos-cli --bin acos
# sampler: M9_ROOT=<worktree> python3 .cache/m9-tools/sample.py {kept N | oneshot N} out.json
#   M9_FAST=1 = rollup-only polling during launch (lower overhead)
#   MALLOC_MMAP_THRESHOLD_=131072 | 1048576 for the allocator experiment
```

## 6. Caveats (stated plainly)

- No ART device or emulator run was done here. The ART comparison in §4 is an estimate.
- Kept daemons (`acos serve` + `launch --keep`) are long-lived, not the one-shot M9 children. Both are reported; the numbers agree at N=5 (533 vs 532.5) and N=20 (527 vs 507, one-shot under-sampled).
- The unnamed ~280 MiB of anon remains unattributed. The census does not tag host-side allocations.
- Sampling cadence is 140–300 ms at N=20, so sub-cadence transients may be missed. The monotone series and the observed 44 MiB dip are the evidence, not an exhaustive trace.
