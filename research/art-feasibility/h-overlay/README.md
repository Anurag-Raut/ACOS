# h-overlay — static RROs on the **stock** ART resource path

E4H (`results/art-feasibility/E4H.md`, finding 3) left one real divergence: the
`Button` renders `INCREMENT` here and `Increment` on the device, because the
emulator applies a **static resource overlay** over `android` and our `Theme`
resolution does not. ACOS-VM solved it in its own `AssetManager`
(`crates/acos-framework-natives`, commit `dbaf79b`, `results/parity/W198A.md`).
This directory answers the same question for the *stock* path: **real
`libandroid_runtime` `AssetManager`/`ApkAssets` on ART, no ACOS resource code and
no hardcoded attribute** — and it works: `SSIM 0.99196 → 0.99685`, `Button#inc`
`267 → 231 px` (= the ART oracle's 231).

Report: `results/art-runtime/P2c.md`. Harness: a fork of E4H's ladder
(`research/art-feasibility/h-view/`) whose only behavioural change is how the
`AssetManager` is built (overlay `ApkAssets` instead of `addAssetPath`) plus a
`sDefaults` seeding fix (§4.2 of the report).

```bash
scripts/build.sh                              # javac + d8 -> /data/dex/hov
scripts/run.sh all                            # all 5 static android RROs applied
HOV_OVERLAY= scripts/run.sh all               # control: stock path, no overlay
HOV_OVERLAY=/data/overlay/PixelConfigOverlayCommon.idmap scripts/run.sh all
python3 scripts/compare.py                    # SSIM/MAE/tree vs ART oracle + ACOS-VM
```

## Overlays: extract once, idmap once (provisioning time)

The five `isStatic && targetPackage="android"` RROs the image ships, from
`product/overlay` via `.cache/boot/android-34/overlays` (tool:
`tools/extract-bootclasspath.sh`, W198A):

| APK | idmap mappings | idmap bytes |
|---|---|---|
| `framework-res__auto_generated_rro_product.apk` | 13 | 460 |
| `GoogleConfigOverlay.apk` | 90 | 976 |
| `GoogleWebViewOverlay.apk` | 1 | 152 |
| `LargeScreenConfigOverlay.apk` | 17 | 284 |
| `PixelConfigOverlayCommon.apk` | 111 | 1084 |

The idmap is produced by the **image's own** `/system/bin/idmap2` (x86_64, runs
in the bwrap mini-root; no reimplementation):

```bash
# one-off, ~0.37 s per overlay end-to-end (all of it sandbox start)
idmap2 create --policy product \
  --target-apk-path /system/framework/framework-res.apk \
  --overlay-apk-path /system/overlay/PixelConfigOverlayCommon.apk \
  --idmap-path /data/overlay/PixelConfigOverlayCommon.idmap
```

`--policy product` is mandatory (see report §4.1): without a policy every entry
is rejected and idmap2 exits with `no resources were overlaid`. Copies of the
five idmaps are in `artifacts/`.

## Runtime: exactly what `ResourcesManager` does at zygote time

```java
ApkAssets host    = ApkAssets.loadFromPath("/system/framework/framework-res.apk");
ApkAssets overlay = ApkAssets.loadOverlayFromPath(idmapPath, ApkAssets.PROPERTY_SYSTEM);
ApkAssets app     = ApkAssets.loadFromPath("/data/apk/counter.apk");
AssetManager am = new AssetManager.Builder()
        .addApkAssets(host).addApkAssets(overlay).addApkAssets(app).build();
```

(API 34 signature is `loadOverlayFromPath(String idmapPath, int loadFlags)`;
`0x1` = `PROPERTY_SYSTEM`, `0x0` also applies the overlay — no measured
difference. `AndroidManifest`-less `AssetManager`s need nothing else.)

| artefact | what it is |
|---|---|
| `java/dev/acos/hov/Main.java` | E4H's ladder + `apkAssets()/apkAssetsOverlay()/assetManagerWithOverlay()` |
| `scripts/build.sh`, `scripts/run.sh`, `scripts/hroot.sh` | build (javac+d8), run under `tools/guarded.sh`, fonts |
| `scripts/compare.py` | SSIM / MAE / exact-pixel / node bounds vs ART oracle **and** ACOS-VM |
| `artifacts/hov-view.png` | the frame with the overlays applied (md5 `2fc7c91d…`, deterministic) |
| `artifacts/hov-view-nooverlay.png` | same harness, `HOV_OVERLAY=` (control, `INCREMENT`, 267 px) |
| `artifacts/hov-{tree,tree-nocaps,tree-nooverlay}.txt` | view trees |
| `artifacts/compare.txt` | comparison output, verbatim |
| `artifacts/*.idmap` | the five idmaps consumed at runtime |

Scratch that is provisioned additively into the E1A sysroot (0 bytes of
duplication, undo with `rm -rf`): `<sysroot>/system/overlay/*.apk` (the five RRO
APKs, hard-linked copies) and `.cache/art-exp/data/overlay/*.idmap`.
