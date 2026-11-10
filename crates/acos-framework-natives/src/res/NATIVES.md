# `android.content.res` natives → engine mapping

This crate is the **VM-independent engine** behind the four classes that carry
`android.content.res` natives: `AssetManager` (21 measured / 58 declared),
`ApkAssets` (3 measured / 12 declared), `StringBlock` (3 / 5) and `XmlBlock`
(11 / 22). There is no `Theme` or `TypedArray` native class — theme methods
live on `AssetManager` and `TypedArray` is pure Java over the `int[]` layout
documented below.

The later native shim maps JNI signatures onto the engine APIs listed here.
"measured" = the number of enters recorded in the API 34 hello-views launch
trace (`research/dynamic-trace/data/natives_activity_launch.tsv`).

## Conventions

* Handles are `u64`; `0` never names a live object. The engine keeps its own
  handle tables (`AssetManager::{assets, blocks, parse_states, themes}`).
* `TypedValue` is written as a plain Rust struct; the shim copies the fields
  into the Java object.
* `outValues`/`outIndices` use the exact `TypedArray` layout:
  `STYLE_NUM_ENTRIES = 7`, `[type, data, assetCookie, resourceId,
  changingConfigurations, density, sourceResourceId]`
  (`res/value.rs`).
* A resource id is `[package:8][type:8][entry:16]`.

## `AssetManager` (all lifetimes are `&self`/`&mut self`)

| native signature | engine | status | measured |
|---|---|---|---|
| `nativeCreate()J` | `AssetManager::new` | complete | 2 |
| `nativeDestroy(J)V` | `Drop` | complete | 0 |
| `nativeSetApkAssets(J[Landroid/content/res/ApkAssets;ZZ)V` | `AssetManager::set_apk_assets` | complete | 2 |
| `nativeSetConfiguration(...)V` (21 args) | `AssetManager::set_configuration(&ConfigurationNative)` | complete | 4 |
| `nativeGetAssignedPackageIdentifiers(JZZ)Landroid/util/SparseArray;` | `AssetManager::assigned_package_identifiers` | complete | 1 |
| `nativeContainsAllocatedTable(J)Z` | `AssetManager::contains_allocated_table` | complete | 0 |
| `nativeList(JLjava/lang/String;)[Ljava/lang/String;` | `AssetSource::list` via `AssetManager::sources` | complete | 0 |
| `nativeOpenAsset(JLjava/lang/String;I)J` | `AssetManager::open_asset` | complete | 0 |
| `nativeOpenAssetFd(...)Landroid/os/ParcelFileDescriptor;` | — | **partial** (reads into memory; no fd) | 0 |
| `nativeOpenNonAsset(JILjava/lang/String;I)J` | `AssetManager::open_non_asset` | complete | 6 |
| `nativeOpenNonAssetFd(...)` | — | **partial** (no fd) | 0 |
| `nativeOpenXmlAsset(JILjava/lang/String;)J` | `AssetManager::open_xml_asset` | complete | 24 |
| `nativeOpenXmlAssetFd(...)J` | — | **missing** (fd path) | 0 |
| `nativeGetResourceValue(JISLandroid/util/TypedValue;Z)I` | `AssetManager::get_resource_value` | complete | 81 |
| `nativeGetResourceBagValue(JIILandroid/util/TypedValue;)I` | `AssetManager::get_resource_bag_value` | complete | 0 |
| `nativeGetStyleAttributes(JI)[I` | `AssetManager::get_style_attributes` | complete | 0 |
| `nativeGetResourceStringArray(JI)[Ljava/lang/String;` | `AssetManager::get_resource_string_array` | complete | 0 |
| `nativeGetResourceStringArrayInfo(JI)[I` | `AssetManager::get_resource_string_array_info` | complete | 0 |
| `nativeGetResourceIntArray(JI)[I` | `AssetManager::get_resource_int_array` | complete | 0 |
| `nativeGetResourceArraySize(JI)I` | `AssetManager::get_resource_array_size` | complete | 0 |
| `nativeGetResourceArray(JI[I)I` | `AssetManager::get_resource_array` | complete | 0 |
| `nativeGetResourceIdentifier(JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;)I` | `AssetManager::get_resource_identifier` | complete | 0 |
| `nativeGetResourceName(JI)Ljava/lang/String;` | `AssetManager::get_resource_name` | complete | 0 |
| `nativeGetResourcePackageName(JI)Ljava/lang/String;` | `AssetManager::get_resource_package_name` | complete | 0 |
| `nativeGetResourceTypeName(JI)Ljava/lang/String;` | `AssetManager::get_resource_type_name` | complete | 5 |
| `nativeGetResourceEntryName(JI)Ljava/lang/String;` | `AssetManager::get_resource_entry_name` | complete | 0 (27 interaction) |
| `nativeGetLocales(JZ)[Ljava/lang/String;` | `AssetManager::get_locales` | complete | 0 |
| `nativeGetSizeConfigurations(J)[Landroid/content/res/Configuration;` | `AssetManager::get_size_configurations` | complete | 1 |
| `nativeGetSizeAndUiModeConfigurations(J)[...]` | `AssetManager::get_size_and_ui_mode_configurations` | complete | 0 |
| `nativeSetResourceResolutionLoggingEnabled(JZ)V` | — | **missing** (diagnostic) | 0 |
| `nativeGetLastResourceResolution(J)Ljava/lang/String;` | — | **missing** (diagnostic) | 0 |
| `nativeAttributeResolutionStack(JJIII)[I` | — | **missing** (diagnostic) | 0 |
| `nativeApplyStyle(JJIIJ[IJJ)V` | `AssetManager::apply_style(theme, defStyleAttr, defStyleRes, parser, attrs, outValues, outIndices)` | complete (diagnostic-free) | 138 |
| `nativeApplyStyleWithArray(...)` | `AssetManager::apply_style` | **partial** | 0 |
| `nativeResolveAttrs(JJII[I[I[I[I)Z` | `AssetManager::resolve_attrs` | complete | 16 |
| `nativeRetrieveAttributes(JJ[I[I[I)Z` | `AssetManager::retrieve_attributes` | complete | 59 |
| `nativeThemeCreate(J)J` | `AssetManager::theme_create` | complete | 8 |
| `nativeGetThemeFreeFunction()J` | — | **missing** (free-function pointer, shim-side) | 0 |
| `nativeThemeApplyStyle(JJIZ)V` | `AssetManager::theme_apply_style` | complete | 8 |
| `nativeThemeRebase(JJ[I[ZI)V` | `AssetManager::theme_rebase` | complete | 0 |
| `nativeThemeCopy(JJJJ)V` | `AssetManager::theme_copy` | complete | 6 |
| `nativeThemeGetAttributeValue(JJILandroid/util/TypedValue;Z)I` | `AssetManager::theme_get_attribute_value` | complete | 4 |
| `nativeThemeDump(JJILjava/lang/String;Ljava/lang/String;)V` | — | **missing** (debug) | 0 |
| `nativeThemeGetChangingConfigurations(J)I` | `ResolvedEntry::changing_configurations` / `PackageIndex::spec_flag` | **partial** (raw mask exposed) | 0 |
| `nativeGetParentThemeIdentifier(JI)I` | `AssetManager::get_parent_theme_identifier` | complete | 0 |
| `nativeAssetDestroy(J)V` | `AssetManager::asset_destroy` | complete | 6 |
| `nativeAssetReadChar(J)I` | `AssetHandle::read_char` | complete | 0 |
| `nativeAssetRead(J[BII)I` | `AssetHandle::read` | complete | 57 |
| `nativeAssetSeek(JJI)J` | `AssetHandle::seek` | complete | 0 |
| `nativeAssetGetLength(J)J` | `AssetHandle::length` | complete | 6 |
| `nativeAssetGetRemainingLength(J)J` | `AssetHandle::remaining_length` | complete | 0 |
| `nativeGetOverlayableMap` / `nativeGetOverlayablesToString` | — | **missing** (overlay subsystem) | 0 |
| `getGlobalAssetCount` / `getAssetAllocations` / `getGlobalAssetManagerCount` | — | **missing** (global debug counters) | 0 |

## `ApkAssets`

| native signature | engine | status | measured |
|---|---|---|---|
| `nativeLoad(ILjava/lang/String;ILandroid/content/res/loader/AssetsProvider;)J` | `ApkAssets::open` | complete (path form) | 6 |
| `nativeLoadEmpty(IL...;)J` | — | **missing** | 0 |
| `nativeLoadFd(...)J` | `ApkAssets::open` | **partial** (no fd form) | 0 |
| `nativeLoadFdOffsets(...)J` | — | **missing** | 0 |
| `nativeDestroy(J)V` | `Drop` | complete | 0 |
| `nativeGetAssetPath(J)Ljava/lang/String;` | `ApkAssets::path` | complete | 0 |
| `nativeGetDebugName(J)Ljava/lang/String;` | `ApkAssets::debug_name` | complete | 0 |
| `nativeGetStringBlock(J)J` | `ResTableIndex::global_strings` / `StringBlock` | complete | 6 |
| `nativeIsUpToDate(J)Z` | `ApkAssets::is_up_to_date` (mtime + size) | complete | 57 |
| `nativeOpenXml(JLjava/lang/String;)J` | `AssetManager::open_xml_asset` | complete | 0 |
| `nativeGetOverlayableInfo(...)` / `nativeDefinesOverlayable(J)Z` | — | **missing** | 0 |

## `StringBlock`

| native signature | engine | status | measured |
|---|---|---|---|
| `nativeCreate([BII)J` | `StringBlock::from_bytes` / `StringPool::parse` | complete | 0 |
| `nativeDestroy(J)V` | `Drop` | complete | 0 |
| `nativeGetSize(J)I` | `StringBlock::len` | complete | 27 |
| `nativeGetString(JI)Ljava/lang/String;` | `StringBlock::get` / `StringPool::get` | complete | 128 |
| `nativeGetStyle(JI)[I` | `StringBlock::get_style` / `StringPool::get_style` | complete | 128 |

`StringPool` decodes UTF-8 and UTF-16 pools and styled spans
(`StyleSpan { name, first_char, last_char }`) lazily; only the header and
offset table are read up front.

## `XmlBlock`

| native signature | engine | status | measured |
|---|---|---|---|
| `nativeCreate([BII)J` | `XmlBlock::from_bytes` / `parse_axml` | complete | 0 |
| `nativeDestroy(J)V` | `AssetManager::destroy_xml_block` | complete | 20 |
| `nativeGetStringBlock(J)J` | `XmlBlock::string_block` | complete | 24 |
| `nativeCreateParseState(JI)J` | `AssetManager::create_parse_state` | complete | 25 |
| `nativeDestroyParseState(J)V` | `AssetManager::destroy_parse_state` | complete | 25 |
| `nativeNext(J)I` | `XmlParserState::native_next` | complete | 226 |
| `nativeGetNamespace(J)I` | `XmlParserState::get_namespace` | complete | 0 |
| `nativeGetName(J)I` | `XmlParserState::get_name` | complete | 143 |
| `nativeGetText(J)I` | `XmlParserState::get_text` | complete | 0 |
| `nativeGetLineNumber(J)I` | `XmlParserState::get_line_number` | complete | 0 |
| `nativeGetAttributeCount(J)I` | `XmlParserState::get_attribute_count` | complete | 22 |
| `nativeGetAttributeNamespace(JI)I` | `XmlParserState::get_attribute_namespace` | complete | 0 |
| `nativeGetAttributeName(JI)I` | `XmlParserState::get_attribute_name` | complete | 0 |
| `nativeGetAttributeResource(JI)I` | `XmlParserState::get_attribute_resource` | complete | 38 |
| `nativeGetAttributeDataType(JI)I` | `XmlParserState::get_attribute_data_type` | complete | 11 |
| `nativeGetAttributeData(JI)I` | `XmlParserState::get_attribute_data` | complete | 11 |
| `nativeGetAttributeStringValue(JI)I` | `XmlParserState::get_attribute_string_value` | complete | 0 |
| `nativeGetIdAttribute(J)I` | `XmlParserState::get_id_attribute` | complete | 0 |
| `nativeGetClassAttribute(J)I` | `XmlParserState::get_class_attribute` | complete | 0 |
| `nativeGetStyleAttribute(J)I` | `XmlParserState::get_style_attribute` | complete | 0 |
| `nativeGetSourceResId(J)I` | `XmlParserState::get_source_res_id` | complete | 11 |
| `nativeGetAttributeIndex(JLjava/lang/String;Ljava/lang/String;)I` | `XmlParserState::get_attribute_index` | complete | 0 |

Event integers match `XmlPullParser`: `START_DOCUMENT=0`, `END_DOCUMENT=1`,
`START_TAG=2`, `END_TAG=3`, `TEXT=4`. `nativeNext` never returns
`START_DOCUMENT` (Java synthesises it on the first `next()`), exactly like
`ResXMLTree::next`.

## Coverage summary

| class | measured natives covered | declared natives covered |
|---|---|---|
| AssetManager | 21/21 | ~46/58 (12 overlay/diagnostic/fd missing) |
| ApkAssets | 3/3 | 9/12 |
| StringBlock | 3/3 | 5/5 |
| XmlBlock | 11/11 | 22/22 |

Everything measured at launch is implemented. The missing entries are the fd
variants (`openAssetFd`, `openXmlAssetFd`), the overlay subsystem
(`nativeGetOverlayableInfo`, `nativeDefinesOverlayable`), diagnostic logging
(`nativeLastResourceResolution`, `nativeAttributeResolutionStack`,
`nativeThemeDump`), global counters and `nativeLoadEmpty`/`nativeLoadFdOffsets`.

## Configuration matching

`res/config.rs` is a port of `ResTable_config::match` + `isBetterThan`:
locale (language/country/script/variant), layout direction, smallest/current
screen dp, screen size/long, orientation, UI mode type + night, density,
touchscreen, keyboard, navigation, input flags, SDK version.

The **density rule** was calibrated against
`aapt2 optimize --target-densities` on real tables: among candidate densities
pick the *smallest density not below the request*; if all are below the request
pick the largest. (e.g. request 190 between 160 and 240 → 240; request 161
between 160 and 640 → 640; request 420 between 320 and 480 → 480.)

## `unsafe` inventory

Exactly one `unsafe` block:

* `assets.rs::ApkAssets::open` — `memmap2::Mmap::map` is unsafe because the
  file could be mutated concurrently. The fixture APKs are read-only; the
  mapping is never written. Documented inline.

Everything else is safe Rust; every parser uses bounds-checked readers
(`res/bytes.rs`, `res/strpool.rs`) and returns `ResError` on malformed input,
so mutation fuzzing (`tests/fuzz_mutation.rs`) cannot panic.
