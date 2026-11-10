# `acos-framework-natives` — resource engine results

Evidence for the VM-independent Rust engine behind the Android framework
`android.content.res` natives (`AssetManager`, `ApkAssets`, `StringBlock`,
`XmlBlock`, themes/styles). Owned by the `res` work item (backlog T-04, T-05,
T-06); see `crates/acos-framework-natives/src/res/NATIVES.md` for the
native → engine mapping.

Real inputs used:

* `.cache/boot/android-34/framework-res.apk` (34,370,676 B, `resources.arsc`
  16,421,724 B, 7,880 zip entries)
* `fixtures/build/apps/hello-views.apk`
* device configuration: 1080×2400, 420 dpi, en-US, port, normal/notnight,
  sdk 34 (`fixtures/reference/hello-views/art-api34-x86_64/device-config.json`)
* oracle: `~/Android/Sdk/build-tools/35.0.0/{aapt,aapt2}`

## Acceptance

### `cargo test -p acos-framework-natives`

```
unittests src/lib.rs                    8 passed; 0 failed
tests/config_matching.rs                7 passed; 0 failed
tests/differential_aapt2.rs             3 passed; 0 failed
tests/fuzz_mutation.rs                  1 passed; 0 failed
tests/memory_lazy.rs                    1 passed; 0 failed
tests/smoke.rs                          3 passed; 0 failed
tests/theme_resolution.rs               2 passed; 0 failed
tests/xmlblock_diff.rs                  3 passed; 0 failed
Doc-tests                               1 passed; 0 failed
```

### `cargo clippy -p acos-framework-natives --all-targets -- -D warnings`

```
    Checking acos-framework-natives v0.1.0 (...)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.07s
```

## Differential vs the platform tools (zero mismatches)

`tests/differential_aapt2.rs` parses `aapt dump --values resources` and compares,
per sampled resource id, the multiset of raw `(type, data)` values, the bag
parent + key sets, the configuration label set, and the value resolved for the
device configuration.

```
DIFFERENTIAL framework-res: checked=1694 value_mismatches=0 label_mismatches=0 bag_mismatches=0
DIFFERENTIAL hello-views:   checked=10   value_mismatches=0 label_mismatches=0 bag_mismatches=0
DIFFERENTIAL device selection: checked=799 mismatches=0
```

* 1,694 framework resource ids checked across every type
  (string/color/dimen/integer/bool/style/layout/drawable/array/attr/...),
  87+ configuration variants for e.g. `string/cancel`.
* Configuration labels reproduce AOSP `ResTable_config::toString()` exactly for
  all sampled configs (locale, `b+sr+Latn` scripts, density, `sw/w/h`, screen
  size, orientation, uiMode/night, `vNN`, mcc/mnc, ...).
* Device-config selection checked on 799 ids with zero mismatches.

`tests/xmlblock_diff.rs` compares the `XmlBlock` event stream to
`acos-apk`'s AXML parser byte-for-byte and the element/attribute structure to
`aapt2 dump xmltree`:

```
XmlBlock res/layout/screen_simple.xml: 8 events, 7 nativeNext calls
XmlBlock res/layout/main.xml:          12 events, 11 nativeNext calls
retrieveAttributes: orientation=1 padding=16dp layout_width=-1
```

(The measured API-34 launch total is **226 `nativeNext`** across the whole
view-inflation path; these are the per-document counts for the two reference
layouts.)

`tests/theme_resolution.rs` resolves the app's device theme against
`aapt dump --values resources` bag data:

```
THEME 0x0103013f: expected=407 compared=407 mismatches=0
colorBackground -> 0xfff0f0f3 type 0x1d
TextAppearance.Small textSize = 14sp -> 36.750px at 420dpi
applyStyle defStyleRes=TextAppearance.Small: textSize type=0x05 data=0x00000e02 index=0
```

### Density rule calibrated against `aapt2 optimize --target-densities`

A 5-bucket probe APK (mdpi/hdpi/xhdpi/xxhdpi/xxxhdpi) was optimized for many
target densities; the engine's rule (smallest density **not below** the
request, else the largest) reproduces aapt2 exactly:

| request | aapt2 selects | engine |
|---|---|---|
| 120 dpi | mdpi (160) | mdpi |
| 190 dpi | hdpi (240) | hdpi |
| 200 dpi | hdpi (240) | hdpi |
| 240 dpi | hdpi (240) | hdpi |
| 245 dpi | xhdpi (320) | xhdpi |
| 300 dpi | xhdpi (320) | xhdpi |
| 420 dpi | xxhdpi (480) | xxhdpi |
| 440 dpi | xxhdpi (480) | xxhdpi |
| 640 dpi | xxxhdpi (640) | xxxhdpi |

`tests/config_matching.rs` covers locale fallback, density (incl. the table
above), smallest width, orientation, night, uiMode type, sdk version, layout
direction and screen size.

## Bench (`cargo bench -p acos-framework-natives`, release)

`results/res/bench.json`; p50 / p95 in ns:

| operation | p50 | p95 | n |
|---|---:|---:|---:|
| `framework_res_load` (mmap + ARSC index) | 392,853 | 897,808 | 10 |
| `getResourceValue` uncached (distinct entries, first chunk scan) | 48,723 | 50,586 | 2,000 |
| `getResourceValue` cached (same id, repeated) | 210 | 230 | 20,000 |
| `applyStyle` for a TextView (`defStyleRes=TextAppearance.Small`) | 762 | 802 | 20,000 |
| `XmlBlock` parse of `screen_simple.xml` | 531 | 561 | 2,000 |
| `XmlBlock` walk (7 `nativeNext` + attribute reads) | 40 | 50 | 2,000 |

Interpretation: the launch-critical paths are sub-microsecond. The uncached
string lookup pays one linear scan of the 2,404 `ResTable_type` chunks for that
type, then the per-(type, entry, config) cache makes it ~200 ns — the same
caching strategy `ResTable` uses.

## Lazy-loading memory (`tests/memory_lazy.rs`, `/proc/self/status`)

```
before            VmHWM= 3,724 kB  VmRSS= 3,724 kB  RssAnon=  288 kB  RssFile= 3,436 kB
after load        VmHWM=17,728 kB  VmRSS=17,728 kB  RssAnon=  360 kB  RssFile=17,368 kB
after 1000 resol. VmHWM=17,864 kB  VmRSS=17,864 kB  RssAnon=  368 kB  RssFile=17,496 kB
heap delta        72 kB after load, 80 kB after 1000 resolutions
```

The APK is memory-mapped, never copied. The anonymous-heap cost is the
lazily-parsed resource-table **index** (type-string/key-string pool headers and
4,064 type-chunk offsets) — 72 KiB. Strings, entries and values are decoded on
demand, so the 16 MB `resources.arsc` and 11 MB string blob are never
materialised. `RssFile` grows because the index walk touches the mapped ARSC
pages (file-backed, reclaimable), not because of heap.

## API summary

* `res::assets` — `AssetSource` trait; `ApkAssets` (mmap/owned bytes, lazy zip
  central-directory scan, zero-copy stored entries), `AssetHandle`
  (read/seek/length/readChar), `is_up_to_date`, path/debug name.
* `res::arsc` — `ResTableIndex` (lazy `ResTable`; packages, type/key pools,
  type-chunk offsets, per-(type, entry, config) best-chunk cache),
  `resolve`, `resolve_bag_entry`, `all_configs`, `array_entries`,
  `style_attributes`, `identifier`, `entry_name/type_name/package_name`,
  `parent_theme_identifier`, `size_configurations`, `all_configurations`,
  `decode_entry`, `lookup_entry_offset`.
* `res::config` — `ResConfig` (`match`, `is_better_than`, `label`,
  density rule, all Android qualifiers).
* `res::strpool` — lazily decoded `StringPool` (UTF-8/UTF-16 + styled spans)
  and an owned `StringBlock`.
* `res::xmlblock` — `XmlBlock`/`XmlDocument`, the AXML parser, and
  `XmlParserState` (`native_next`, name/namespace/text/line, attribute
  name/namespace/resource/type/data/string, id/class/style indexes,
  `get_attribute_index`).
* `res::theme` — `Theme` (apply/parent chain/`?attr`/copy/rebase),
  `AssetManager::{apply_style, resolve_attrs, retrieve_attributes}` producing
  the exact 7-int `TypedArray` layout.
* `res::manager` — `AssetManager`: layered sources, cookies, configuration,
  resource lookups with reference chasing, asset/XML/theme handle tables.

## Coverage / everything partial

`NATIVES.md` has the full table. Summary: **all measured natives are
implemented** (AssetManager 21/21, ApkAssets 3/3, StringBlock 3/3,
XmlBlock 11/11). Still missing (none on the hello-views launch path):

* fd-returning variants (`nativeOpenAssetFd`, `nativeOpenNonAssetFd`,
  `nativeOpenXmlAssetFd`, `nativeLoadFd`, `nativeLoadFdOffsets`) — the engine
  reads into memory instead of handing out `ParcelFileDescriptor`s.
* overlay subsystem (`nativeGetOverlayableInfo`, `nativeDefinesOverlayable`,
  `nativeGetOverlayableMap`, `nativeGetOverlayablesToString`).
* diagnostics/global counters (`nativeSetResourceResolutionLoggingEnabled`,
  `nativeGetLastResourceResolution`, `nativeAttributeResolutionStack`,
  `nativeThemeDump`, `getGlobalAssetCount`, `getGlobalAssetManagerCount`,
  `nativeLoadEmpty`).
* `nativeThemeGetChangingConfigurations` — the raw spec mask is exposed
  (`ResolvedEntry::changing_configurations`); the Java `ActivityInfo`
  conversion is a shim-side concern.
* `nativeGetResourceArray` uses the resolved best bag; the exotic
  `getResourceStringArrayInfo` string-index path is implemented but untested
  against a reference.
* `nativeApplyStyle`'s `outIndices` reports the *source* index (XML tag /
  `defStyleAttr` / `defStyleRes` / theme) rather than the index of the entry
  inside that source bag; `nativeRetrieveAttributes` reports the XML attribute
  index. `TypedArray.getIndex` consumers should be re-checked against ART.

## `unsafe`

One block: `ApkAssets::open` calls `memmap2::Mmap::map`. Everything else is
safe; all parsing is bounds-checked and returns `ResError`, and
`tests/fuzz_mutation.rs` mutates ARSC/AXML/string-pool inputs ~6,500 times
without a panic.
