# E2E — Which seam do we cut so that real framework views get drawn by `acos-render`?

**Answer: cut the seam at the Skia CPU canvas, not at the JNI natives.**
Under real ART + the real `framework.jar` classes + **real `libhwui.so`**, a
Bitmap-backed `Canvas` already produces correct pixels today — measured, no
Android boot, no zygote, no GPU:

```
DRAW-OK 64x64 nonzero=3136 exactRed=2804 exactGreen=276
       px(0,0)=00000000 px(4,4)=ffff0000 px(32,32)=ff00ff00   (transparent / red rect / green disc)
drawRect x1000: 0.30 ms (0.30 us/op)     bitmap+canvas create: 7.7 us/op
RenderNode+RecordingCanvas begin/endRecording: OK w=100 h=100
peak dalvikvm RSS: 163 MB (ART + libhwui + libandroid_runtime loaded)
```

The enabling discovery is not a hack: **`libhwui.so` exports
`register_android_graphics_classes(JNIEnv*)`** (version `LIBHWUI`) and
**`libandroid_runtime.so` exports 131 `register_android_*` functions** — the
framework's entire JNI registration surface, callable from a plain
`dlopen`ed bridge. So the "intercept the natives" option (a) is *not* needed to
get the real engine running, and option (b)'s "run real libhwui" is no longer a
research question but an integration question.

- Owner / date: graphics-seam agent (E2), 2026-11-11. Time used ~2 h (time-box 45 min; the runtime chain
  needed 11 iterations to first pixels — §4 keeps every blocker).
- Constraints honoured: no ACOS source crate modified, nothing committed, no
  sudo, nothing outside `~/Projects/acos`, host RAM peak 163 MB (measured),
  long runs through `tools/guarded.sh`. Scratch: `.cache/art-exp-e/`.
- Depends on **E1A** (sysroot + `artroot.sh`) and reuses **E1C**'s
  `System.load("/data/lib/*.so")` recipe.

Reproduce end-to-end (rebuilds the bridge, the dex, applies the one sysroot
permission change, runs under the guard):

```bash
research/art-feasibility/e-graphics/scripts/build-run.sh
```

| artefact | what it is |
|---|---|
| `scripts/jninatives.py` | extracts **JNINativeMethod tables** from an Android `.so` with no symbols: unpacks `SHT_ANDROID_RELA` (APS2), reads `.data.rel.ro`/`.data` triples, validates `{name,sig,fn}` structurally |
| `scripts/attrib.py` | attributes each recovered table to its Java class by locating the RIP-relative `lea` of the table address in `.text` and the nearest `android/graphics/...` literal |
| `natives_full.json` | the **888** natives recovered from `libhwui.so` (per-entry scan) |
| `jninatives.json` | all **288** tables in `libhwui.so` (848 natives) and `libandroid_runtime.so` (2 358 natives) |
| `e2/bridge.c`, `e2/natives.h`, `e2/classes.h` | the JNI bridge: `dlopen` libhwui/libandroid_runtime, call the real registration entry points (and, as a fallback path, re-register the extracted tables) |
| `e2/java/dev/acos/e2/*.java` | the probe: enumerate declared natives, `Bitmap.createBitmap` → `new Canvas(bm)` → `drawRect`/`drawCircle` → `getPixels` → assert/dump; RenderNode + RecordingCanvas round trip |
| `scripts/public-libs.sh` | the one environment change the seam needs (bionic namespace permission, §4 B1); `--restore` undoes it |

## 1. The JNI surface of the real graphics stack (measured by extraction)

`libhwui.so` is 10.2 MB and contains **Skia statically linked** (there is **no
`libskia.so` anywhere in the API-34 image** — verified by `grep`ing the whole
sysroot). All drawing natives are registered *by name* from hidden functions;
no `Java_android_graphics_*` symbol is exported, which is why a naive
`System.load` of libhwui cannot satisfy any graphics native.

Recovered from `libhwui.so`: **888** `(name, signature, code offset)` entries
(827 unique name+signature) laid out in **68** tables. The ones that matter for the seam:

| Java class | natives | examples | Rust already implements (see §2) |
|---|---|---|---|
| `android.graphics/Canvas` | 62 | `nInitRaster(J)J`, `nSetBitmap(JJ)V`, `nDrawRect(JFFFFJ)V`, `nDrawBitmap(JJFFJIII)V`, `nDrawText(JLs…;IIFFIJ)V`, `nSaveLayer(JFFFFJ)I` | 58/58 registered names |
| `android.graphics/RenderNode` | **89** | `nCreate(Ljava/lang/String;)J`, `nSetPosition`, `nSetAlpha`, `nAddAnimator`, `nGetAllocatedSize` | 92 |
| `android.graphics/HardwareRenderer` | **54** | `nCreateRootRenderNode()J`, `nCreateProxy(ZJ)J`, `nAddRenderNode(JJZ)V`, `nDrawRenderNode(JJ)V`, `nCreateTextureLayer`, `nAllocateBuffers` | 74 (`gfx_hwui.rs`) |
| `android.graphics/RecordingCanvas` | 12 | `nCreateDisplayListCanvas(JII)J`, `nResetDisplayListCanvas`, `nFinishRecording`, `nEnableZ`, `nDrawRenderNode` | 12/12 |
| `android.graphics/Bitmap` (+ Paint/text in the same run) | 89 | `nativeCreate([IIIIIIZJ)Landroid/graphics/Bitmap;`, `nInit`, `nSetColor`, `nBreakText`, `nGetTextAdvances` | 48 (`gfx_bitmap`) |
| `android.graphics/Matrix` | 42 | `nSetConcat(JJJ)V`, `nMapRect` | 42/42 |
| `android.graphics/Path` | 33 | `nInit`, `nMoveTo`, `nApproximate`, `nGetBounds` | 57 |
| `android.graphics/Typeface` | 9 | `nativeCreateFromTypeface`, `nativeCreateFromFamiliesWithDefault` | 34 (`gfx_text`) |
| `android.graphics/VectorDrawable` (merged run) | 71 | `nCreateTree`, `nUpdateFullPathProperties`, `nDraw` | 76 (`gfx_vector`) |
| `android.graphics/Region` / `RegionIterator` / `Color` / `Camera` / `Movie` / `Mesh` | 19/3/3/19/6/7 | — | 34 (`gfx_ripple`), 92 (`gfx_node`) |

Declared-native side (real `framework.jar`, i.e. what the framework will
actually call): scanning the **456** distinct `android/graphics/*` classes in the
image's framework dex found **956** `native` method declarations; restricting to
the **47** classes the graphics path uses gives **620** declared natives, and
`libhwui` provides **620/620** of them (exact name+signature). There is **no
graphics native in the framework that libhwui does not implement** — the JNI
surface is complete and closed.

`libandroid_runtime.so` (2.5 MB, 2 358 natives in 220 tables) holds the rest of
the framework natives the graphics path leans on (`SystemProperties`,
`DisplayMetrics`, `Log`, `CharsetUtils`, sensors, GLES, …), also exported as
`register_android_*`.

## 2. How much of that does ACOS already implement in Rust?

Important correction to the brief: **`crates/acos-framework-natives` contains no
graphics code at all** — it is the `android.content.res` engine (AssetManager,
ApkAssets, StringBlock, XmlBlock, themes). The graphics natives live in
**`crates/acos-vm/src/natives/gfx_*.rs`** (12 files, 10 589 lines).

Measured by diffing the exact `(name, signature)` pairs the Rust VM registers
against the pairs recovered from the real image (`natives_full.json` +
`jninatives.json`):

| Rust module | registered | exactly present in the real libs |
|---|---|---|
| `gfx_canvas.rs` | 58 | **58** |
| `gfx_node.rs` (RenderNode) | 92 | **92** |
| `gfx_hwui.rs` (HardwareRenderer/Threads) | 74 | **74** |
| `gfx_vector.rs` | 76 | **76** |
| `gfx_path.rs` | 57 | **57** |
| `gfx_bitmap.rs` | 48 | **48** |
| `gfx_matrix.rs` | 42 | **42** |
| `gfx_ripple.rs` | 34 | **34** |
| `gfx_paint.rs` | 87 | 84 |
| `gfx_text.rs` | 34 | 33 |
| **graphics total** | **602** | **598 (99.3 %)** |
| whole VM (`natives/*.rs`, 62 modules) | 1 601 | 864 (54.0 %) |

So the Rust side already speaks the exact JNI ABI of the drawing surface — the
gap for option (a) is a JNI shim and the *semantics behind* the natives
(display-list recording, HWUI effects, Minikin text), not the surface.

## 3. The three seams, with what was measured

**(c) Canvas software draw into a Bitmap — WORKS TODAY (best fidelity/effort).**
Real `android.graphics.Canvas` + real `libhwui` `SkiaCanvas` + real Skia CPU
raster, drawing into a real `Bitmap`, then `getPixels`. Correct pixels,
0.30 µs per `drawRect`, 7.7 µs per bitmap+canvas pair, 163 MB peak RSS.
This is exactly the `ViewRootImpl` **software draw path**
(`ThreadedRenderer.isEnabled()==false` → `ViewRootImpl.drawSoftware` →
`Canvas` on a `Bitmap`/`Surface`), i.e. an Android-supported, first-class path —
not a hack. Skia does the 2D work; ACOS only has to own
framebuffer+compositor+window (which `acos-render`/`acos-wm` already own).

**(b) real libhwui with a software (SkiaCpu) *display-list* pipeline — feasible,
unmeasured headroom.** The pixel path above proves libhwui's CPU raster works
under E1A. What is *not* proven is HWUI's own display-list rasterization
(`RenderNode` → `SkiaDisplayList` → `SkSurface`) and `HardwareRenderer`
(`RenderProxy`, `RenderThread`), which pull `AChoreographer`/`SysChoreographer`
(`libnativedisplay`), `EGL`/`GLES`/`Vulkan` and a `Surface`; on this host there
is no GPU and no display service, so the hardware path is expected to dead-end.
`RenderNode` + `RecordingCanvas.beginRecording/endRecording` already work
(measured), so the *recording* half is free; the *raster* half is the risk.

**(a) intercept the natives and feed `acos-render` — possible, most work, most
divergence.** Viable because (i) the surface is complete and closed (620/620),
(ii) the Rust VM already implements 598/603 of it, and (iii) `RegisterNatives`
from an NDK-built `.so` inside real ART is proven (E1C + here: 620 graphics
natives registered from our extracted tables). The measured trap: hand
registration is **not sufficient** — libhwui's native bodies depend on
class-global refs cached by the register functions:

```
JNI DETECTED ERROR IN APPLICATION: java_class == null
  in call to NewObjectV
  from android.graphics.Bitmap android.graphics.Bitmap.nativeCreate(int[],…)   (measured)
```

so option (a) either re-implements the whole native surface (which is what it
means to "own the renderer") or must call the exported `register_android_*`
first — at which point it is competing with (c) for the same pixels.

## 4. Blockers, exactly (all encountered, all cheap)

| id | blocker | measured symptom | fix (used) | product cost |
|---|---|---|---|---|
| B1 | bionic namespace: a `/data` lib lives in `clns-1` (`permitted_paths=/data`) and may not `dlopen` `/system/lib64/*` | `library "/system/lib64/libhwui.so" … is not accessible for the namespace: [name="clns-1", …]` | add `libhwui.so`/`libandroid_runtime.so` to `/system/etc/public.libraries.txt` (scratch sysroot) — `scripts/public-libs.sh` | **1** config line per lib in our image, or run the bridge from the *system* namespace (a `app_process`-style launcher) — 0.5 d |
| B2 | libhwui exports no `Java_*`; its natives must be registered | native → `UnsatisfiedLinkError` | call the exported `register_android_graphics_classes(JNIEnv*)`; **do not** hand-register tables (B3) | 0 (measured) |
| B3 | hand-registered natives crash: class-global refs are set by the register function | `JNI DETECTED ERROR … java_class == null … NewObjectV` from `Bitmap.nativeCreate` | use B2's entry point | 0 |
| B4 | `Bitmap.createBitmap` pulls `DisplayMetrics` → `SystemProperties.native_get_int` → `libandroid_runtime` registration | `UnsatisfiedLinkError: android.os.SystemProperties.native_get_int` | `dlopen("libandroid_runtime.so")` (needs B1) + call its `register_android_*` (131 exported, C++-mangled `_ZN7android…register_android_…P7_JNIEnv`) | 0.5 d to build the registration order list |
| B5 | mass `RegisterNatives` probing (888 natives × 456 classes) **crashes ART** with no tombstone | exit 139 / `SIGSEGV`, `libc: failed to connect to tombstoned`; ART also dumps every class for every failure | never probe; register only *declared* natives, or call B2 | — |
| B6 | `HardwareRenderer`'s own pipeline needs a display + GPU | not measured (no GPU, no display service in E1A root) | expected: unavailable; use (c) | risk for (b)'s hardware half |

## 5. Seam comparison

Effort in agent-days, estimated from what was *measured* here (the hard parts of
(c) are already done; (a)'s surface is already 99.3 % written in Rust).

| seam | fidelity (visual + behavioural) | effort (agent-days) | blockers | recommendation |
|---|---|---|---|---|
| **(c) real Skia CPU canvas → Bitmap → `acos-wm` buffer** — force the `ViewRootImpl` software path, `register_android_graphics_classes`, present the Bitmap into the compositor | **highest** for 2D: real Skia raster, real `Paint`/`Path`/`Shader`/`Matrix`, real text via Minikin/HarfBuzz, correct AA/blend/gradients; loses GPU effects (`RenderEffect`, hardware layers, `RenderNode` blurs) | **2–4** (measured: 3 h to first pixels + 1 d hardening + 1–2 d `ViewRootImpl`/`Surface` integration: `Bitmap` → `acos-render` upload) | B1, B4 only | **PRIMARY.** Ship this for M-B/M-C; it *is* the real engine for the 2D surface, and it keeps `acos-render`/`acos-wm` owning presentation |
| **(b) real libhwui display-list raster (SkiaCpu) + RenderThread bypass** | same 2D fidelity as (c), plus HWUI's own display-list/effect pipeline (`RenderNode` casts/shadows, saveLayer bookkeeping) if we can drive `SkiaDisplayList::draw` on a CPU `SkSurface` | **8–15** (recording half already works; raster/orchestration `renderjob` + `SysChoreographer` stubs + surface plumbing are the work) | B1, B4, B6 (choreographer/EGL), unmeasured HWUI-internals risk (`RenderProxy` assumes a real `Surface`) | **STRETCH**, only if (c) shows visual gaps on `RenderNode`-based effects (elevation/shadows) |
| **(a) intercept `android.graphics.*`/`RenderNode`/`HardwareRenderer` natives → `acos-render`** | **we choose it**: full control and determinism, and the Rust bodies already exist (598/603 exact ABI matches); but every pixel is ours to match — Minikin metrics, Skia AA, HWUI effect maths become divergence risk | **6–12** (JNI shim from `gfx_*.rs` 1–2 d; display-list capture/`RenderNode` property sync 3–5 d; text 2–3 d; ripple/vector/effects 2 d) + ongoing parity work | B3/B5 if done naively; none fundamental | **KEEP as kill-switch + oracle.** Its unique value now is a *differential test*: run (c) and (a) on the same display list and diff pixels — that is a much better parity gate than golden PNGs |
| (a′) hybrid: real framework classes, **our** natives only where we want control (text, ripple), real libhwui natives elsewhere | high | 3–5 | B1–B4 | optional later; needs per-class registration control, which B2's single entry point does not give — would need the extracted tables **plus** the register function (both are in `natives_full.json`) |

**Recommended cut:** for every frame, the framework records as it does on device;
ACOS forces the software draw path so the recorder's target is a `Bitmap` that
**`acos-render` owns** (we allocate it, we keep the pixels), then
`acos-wm`/`acos-render` composites that buffer. Real ART, real framework, real
Skia; ACOS keeps the buffer, the damage model, the compositor and the window
manager. Do **not** re-implement the natives for V1; keep the Rust natives as a
byte-exact oracle.

## 6. Numbers, one place

| measurement | value |
|---|---|
| `libhwui.so` natives recovered (per-entry) | **888** (68 tables; 848 from contiguity-only scan) |
| `libandroid_runtime.so` natives recovered | **2 358** (220 tables); 131 `register_android_*` exported |
| `android/graphics/*` classes in framework dex / declared natives | 456 / 956 (620 declared across the 47-class graphics subset) |
| graphics natives libhwui provides for those 47 classes | **620 / 620** |
| ACOS Rust graphics natives / exact ABI match | **602 / 598 (99.3 %)** |
| ACOS Rust natives overall / exact ABI match | 1 601 / 864 (54.0 %) |
| first pixels (Bitmap+Canvas+drawRect+drawCircle+getPixels) | **1.86 ms** wall inside ART |
| `drawRect` 8×8, 1 000 iterations | **0.30 µs/op** |
| `createBitmap` + `new Canvas` | **7.7 µs/op** |
| `RenderNode` + `beginRecording`/`endRecording` | OK (w=100, h=100) |
| peak `dalvikvm64` RSS with libhwui + libandroid_runtime loaded | **163 MB** |
| full process: ART init → libhwui load → registration → draw → pixels → exit | **0.635 s wall** (E1A baseline: 177 ms) |
| registration cost | one `register_android_graphics_classes(env)` call (no per-native probing; the mass-probing variant took 6.06 s and then crashed — B5) |

## 7. Not measured / open

- HWUI's `SkiaDisplayList` raster and `RenderProxy`/`RenderThread` under this
  root (no GPU, no display, no `SysChoreographer` service) — the (b) estimate is
  an integration estimate, not a measurement.
- Text quality through the real path (needs `AssetManager`/font config from the
  framework; `Typeface` natives registered fine but no text was drawn — the
  E1A root has no `/product/fonts` bind yet).
- Damage/present side: the probe returns pixels; nobody has yet pushed a
  `Bitmap` into `acos-wm`'s frame sink (W19-A `FrameSink`) — 1–2 d, first thing
  to do after this report.
- Determinism: the ART path gives up virtual time (DECISION_ART_RUNTIME); Skia's
  AA is deterministic given the same inputs, so pixel-diff gates stay usable.
