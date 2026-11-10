//! Real `android.content.res` natives backed by the `acos-framework-natives`
//! resource engine (`crates/acos-framework-natives/src/res`).
//!
//! The engine is VM-independent: it exposes `AssetManager`, `ApkAssets`,
//! `XmlBlock`, `StringBlock`, `Theme` and the `TypedValue`/`TypedArray` 7-int
//! layout as plain Rust structs over integer handles. This module is the shim
//! that maps the framework's JNI signatures onto those APIs and moves values in
//! and out of Java objects.
//!
//! # State
//!
//! The engine keeps a handle table per `AssetManager`. The shim adds a second,
//! *global* namespace so the static natives (which do not receive an
//! `AssetManager`) can always find their owner:
//!
//! * `managers`      – `AssetManager` engine instances.
//! * `apk_assets`    – `ApkAssets` engine instances (`ApkAssets.nativeLoad`).
//! * `string_blocks` – standalone `StringBlock` payloads.
//! * `block_refs`    – global `XmlBlock` refs plus the manager that owns them.
//! * `parse_refs`    – global `XmlParserState` refs.
//!
//! The state is per-OS-thread (`Vm` is `!Send`, so one interpreter runs on one
//! host thread), which also means the shim never holds a lock across a call
//! back into the VM.
//!
//! # Coverage
//!
//! Everything the API 34 hello-views launch trace actually enters is
//! implemented. The fd variants, overlay subsystem, diagnostic logging and
//! global debug counters are deliberately left unregistered so the VM surfaces
//! them as structured `UNSUPPORTED_JAVA_API` rather than faking a result.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use acos_framework_natives::res::assets::{ApkAssets, AssetSource};
use acos_framework_natives::res::config::ResConfig;
use acos_framework_natives::res::manager::{AssetManager, ConfigurationNative};
use acos_framework_natives::res::strpool::StringBlock;
use acos_framework_natives::res::value::TypedValueOut;
use acos_framework_natives::res::xmlblock::{XmlBlock, XmlDocument, XmlParserState};

use crate::class::{NativeCtx, NativeFn};
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, Handle, Object, NULL};
use crate::javabridge::JavaValue;
use crate::vm::Vm;

const INT: &str = "I";
const STRING: &str = "Ljava/lang/String;";
const CONFIGURATION: &str = "Landroid/content/res/Configuration;";

/// How an `ApkAssets` engine object was created. The engine object is not
/// `Clone`, so the path is retained and the manager re-opens its own copy.
#[derive(Debug, Clone)]
enum ApkKind {
    /// Loaded from a path (possibly a `/system/...` guest path).
    Path(String),
    /// `nativeLoadEmpty` — an empty asset source.
    Empty,
}

struct ApkEntry {
    assets: ApkAssets,
    kind: ApkKind,
}

/// A global `XmlBlock` reference and its owning engine manager.
#[derive(Clone, Copy, Debug)]
struct BlockRef {
    manager: u64,
    block: u64,
}

/// A global `XmlParserState` reference, the block it parses, and its owner.
#[derive(Clone, Copy, Debug)]
struct ParseRef {
    manager: u64,
    block: u64,
    parse: u64,
}

/// The per-thread native state.
#[derive(Default)]
struct Registry {
    managers: HashMap<u64, AssetManager>,
    apk_assets: HashMap<u64, ApkEntry>,
    string_blocks: HashMap<u64, StringBlock>,
    block_refs: HashMap<u64, BlockRef>,
    parse_refs: HashMap<u64, ParseRef>,
    /// Lazily-created manager for the static `XmlBlock.nativeCreate` and
    /// `ApkAssets.nativeOpenXml` paths, whose blocks have no framework owner.
    orphan: Option<u64>,
    /// Open `AAssetDir` iteration cursors, keyed by a registry-local id.
    dirs: HashMap<u64, (Vec<String>, usize)>,
    next: u64,
}

impl Registry {
    fn alloc(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn orphan_manager(&mut self) -> u64 {
        if let Some(h) = self.orphan {
            return h;
        }
        let h = self.alloc();
        self.managers.insert(h, AssetManager::new());
        self.orphan = Some(h);
        h
    }
}

thread_local! {
    static REG: RefCell<Registry> = RefCell::new(Registry::default());
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn internal(msg: impl Into<String>) -> VmError {
    VmError::Internal(msg.into())
}

fn map_path_roots(boot: &Path, root: &Path, path: &str) -> PathBuf {
    // Installed app APKs live under the writable session sandbox; the guest
    // path is the same path the framework sees in `ApplicationInfo.sourceDir`.
    if path.starts_with("/data/") {
        return root.join(path.trim_start_matches('/'));
    }
    if let Some(rest) = path.strip_prefix("/system/") {
        return match rest {
            "framework/framework-res.apk" => boot.join("framework-res.apk"),
            "etc/fonts.xml" | "etc/system_fonts.xml" => boot.join("fonts").join("fonts.xml"),
            "etc/preloaded-classes" => boot.join("preloaded-classes"),
            other => boot.join(other),
        };
    }
    if let Some(rest) = path.strip_prefix("/apex/com.android.tzdata/") {
        return boot.join("apex").join("com.android.tzdata").join(rest);
    }
    PathBuf::from(path)
}

fn detach(vm: &mut Vm, h: Handle) {
    let _ = vm.release_bridge_root(h);
}

fn set_int(vm: &mut Vm, obj: Handle, name: &str, v: i32) {
    let _ = vm.set_field(obj, name, JavaValue::Int(v));
}

fn byte_span(vm: &Vm, h: Handle, off: usize, len: usize) -> Option<Vec<u8>> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Byte(v) => {
                let end = off.checked_add(len)?;
                if end > v.len() {
                    return None;
                }
                Some(v[off..end].iter().map(|&b| b as u8).collect())
            }
            _ => None,
        },
        _ => None,
    }
}

fn read_ints(vm: &Vm, h: Handle) -> Option<Vec<i32>> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Int(v) => Some(v.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn read_bools(vm: &Vm, h: Handle) -> Option<Vec<bool>> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Bool(v) => Some(v.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn write_ints(vm: &mut Vm, h: Handle, vals: &[i32]) -> bool {
    match vm.heap.get_mut(h) {
        Some(Object::Array(a)) => match &mut a.data {
            ArrayData::Int(v) => {
                let n = v.len().min(vals.len());
                v[..n].copy_from_slice(&vals[..n]);
                true
            }
            _ => false,
        },
        _ => false,
    }
}

fn write_bytes(vm: &mut Vm, h: Handle, off: usize, data: &[u8]) -> bool {
    match vm.heap.get_mut(h) {
        Some(Object::Array(a)) => match &mut a.data {
            ArrayData::Byte(v) => {
                let end = off.saturating_add(data.len());
                if end > v.len() {
                    return false;
                }
                for (i, &b) in data.iter().enumerate() {
                    v[off + i] = b as i8;
                }
                true
            }
            _ => false,
        },
        _ => false,
    }
}

fn int_array(vm: &mut Vm, vals: Vec<i32>) -> VmResult<Handle> {
    vm.alloc(Object::Array(ArrayObj {
        elem_desc: INT.to_string(),
        data: ArrayData::Int(vals),
    }))
}

fn string_array(vm: &mut Vm, vals: &[Option<String>]) -> VmResult<Handle> {
    let mut refs = Vec::with_capacity(vals.len());
    for v in vals {
        match v {
            Some(s) => refs.push(vm.new_string(s)?),
            None => refs.push(NULL),
        }
    }
    vm.alloc(Object::Array(ArrayObj {
        elem_desc: STRING.to_string(),
        data: ArrayData::Ref(refs),
    }))
}

fn write_typed_value(vm: &mut Vm, tv: Handle, out: &TypedValueOut) {
    set_int(vm, tv, "type", out.data_type as i32);
    set_int(vm, tv, "data", out.data as i32);
    set_int(vm, tv, "assetCookie", out.asset_cookie);
    set_int(vm, tv, "resourceId", out.resource_id as i32);
    set_int(
        vm,
        tv,
        "changingConfigurations",
        out.changing_configurations,
    );
    set_int(vm, tv, "density", out.density as i32);
    set_int(vm, tv, "sourceResourceId", out.source_resource_id as i32);
}

fn config_to_java(vm: &mut Vm, c: &ResConfig) -> VmResult<Handle> {
    let h = vm
        .new_instance("android.content.res.Configuration")
        .map_err(|e| internal(format!("Configuration: {e}")))?;
    set_int(vm, h, "mcc", c.mcc as i32);
    set_int(vm, h, "mnc", c.mnc as i32);
    set_int(vm, h, "orientation", c.orientation as i32);
    set_int(vm, h, "touchscreen", c.touchscreen as i32);
    set_int(vm, h, "densityDpi", c.density as i32);
    set_int(vm, h, "keyboard", c.keyboard as i32);
    set_int(vm, h, "keyboardHidden", (c.input_flags & 0x03) as i32);
    set_int(vm, h, "navigation", c.navigation as i32);
    set_int(vm, h, "screenWidthDp", c.screen_width_dp as i32);
    set_int(vm, h, "screenHeightDp", c.screen_height_dp as i32);
    set_int(
        vm,
        h,
        "smallestScreenWidthDp",
        c.smallest_screen_width_dp as i32,
    );
    set_int(vm, h, "screenLayout", c.screen_layout as i32);
    set_int(vm, h, "uiMode", c.ui_mode as i32);
    set_int(vm, h, "colorMode", c.color_mode as i32);
    let _ = vm.set_field(h, "fontScale", JavaValue::Float(1.0));
    detach(vm, h);
    Ok(h)
}

fn config_array(vm: &mut Vm, configs: &[ResConfig]) -> VmResult<Handle> {
    let mut refs = Vec::with_capacity(configs.len());
    for c in configs {
        refs.push(config_to_java(vm, c)?);
    }
    vm.alloc(Object::Array(ArrayObj {
        elem_desc: CONFIGURATION.to_string(),
        data: ArrayData::Ref(refs),
    }))
}

fn string_block_from_apk(entry: &ApkEntry) -> Option<StringBlock> {
    let table = entry.assets.resource_table()?;
    let bytes = entry.assets.resource_bytes()?;
    let pool = table.global_strings(bytes).ok()?;
    StringBlock::from_pool(&pool).ok()
}

/// Dispatch a read-only XML query to the manager that owns `pref`.
fn with_parse<T>(pref: u64, f: impl FnOnce(&XmlParserState, &XmlDocument) -> T) -> Option<T> {
    REG.with(|r| {
        let reg = r.borrow();
        let p = reg.parse_refs.get(&pref)?;
        let m = reg.managers.get(&p.manager)?;
        let s = m.parse_state(p.parse)?;
        let b = m.xml_block(p.block)?;
        Some(f(s, &b.doc))
    })
}

fn alloc_block(reg: &mut Registry, manager: u64, block: u64) -> u64 {
    let r = reg.alloc();
    reg.block_refs.insert(
        r,
        BlockRef {
            manager,
            block,
        },
    );
    r
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let am = "Landroid/content/res/AssetManager;";
    let apk = "Landroid/content/res/ApkAssets;";
    let sb = "Landroid/content/res/StringBlock;";
    let xb = "Landroid/content/res/XmlBlock;";

    reg(vm, am, "nativeCreate", "()J", am_create);
    reg(vm, am, "nativeDestroy", "(J)V", am_destroy);
    reg(
        vm,
        am,
        "nativeSetApkAssets",
        "(J[Landroid/content/res/ApkAssets;Z)V",
        am_set_apk_assets,
    );
    reg(
        vm,
        am,
        "nativeSetConfiguration",
        "(JIILjava/lang/String;IIIIIIIIIIIIIIII)V",
        am_set_configuration,
    );
    reg(
        vm,
        am,
        "nativeGetAssignedPackageIdentifiers",
        "(JZZ)Landroid/util/SparseArray;",
        am_get_assigned_package_identifiers,
    );
    reg(
        vm,
        am,
        "nativeContainsAllocatedTable",
        "(J)Z",
        am_contains_allocated_table,
    );
    reg(
        vm,
        am,
        "nativeList",
        "(JLjava/lang/String;)[Ljava/lang/String;",
        am_list,
    );
    reg(
        vm,
        am,
        "nativeOpenAsset",
        "(JLjava/lang/String;I)J",
        am_open_asset,
    );
    reg(
        vm,
        am,
        "nativeOpenNonAsset",
        "(JILjava/lang/String;I)J",
        am_open_non_asset,
    );
    reg(
        vm,
        am,
        "nativeOpenXmlAsset",
        "(JILjava/lang/String;)J",
        am_open_xml_asset,
    );
    reg(
        vm,
        am,
        "nativeGetResourceValue",
        "(JISLandroid/util/TypedValue;Z)I",
        am_get_resource_value,
    );
    reg(
        vm,
        am,
        "nativeGetResourceBagValue",
        "(JIILandroid/util/TypedValue;)I",
        am_get_resource_bag_value,
    );
    reg(
        vm,
        am,
        "nativeGetStyleAttributes",
        "(JI)[I",
        am_get_style_attributes,
    );
    reg(
        vm,
        am,
        "nativeGetResourceStringArray",
        "(JI)[Ljava/lang/String;",
        am_get_resource_string_array,
    );
    reg(
        vm,
        am,
        "nativeGetResourceStringArrayInfo",
        "(JI)[I",
        am_get_resource_string_array_info,
    );
    reg(
        vm,
        am,
        "nativeGetResourceIntArray",
        "(JI)[I",
        am_get_resource_int_array,
    );
    reg(
        vm,
        am,
        "nativeGetResourceArraySize",
        "(JI)I",
        am_get_resource_array_size,
    );
    reg(
        vm,
        am,
        "nativeGetResourceArray",
        "(JI[I)I",
        am_get_resource_array,
    );
    reg(
        vm,
        am,
        "nativeGetResourceIdentifier",
        "(JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;)I",
        am_get_resource_identifier,
    );
    reg(
        vm,
        am,
        "nativeGetResourceName",
        "(JI)Ljava/lang/String;",
        am_get_resource_name,
    );
    reg(
        vm,
        am,
        "nativeGetResourcePackageName",
        "(JI)Ljava/lang/String;",
        am_get_resource_package_name,
    );
    reg(
        vm,
        am,
        "nativeGetResourceTypeName",
        "(JI)Ljava/lang/String;",
        am_get_resource_type_name,
    );
    reg(
        vm,
        am,
        "nativeGetResourceEntryName",
        "(JI)Ljava/lang/String;",
        am_get_resource_entry_name,
    );
    reg(
        vm,
        am,
        "nativeGetLocales",
        "(JZ)[Ljava/lang/String;",
        am_get_locales,
    );
    reg(
        vm,
        am,
        "nativeGetSizeConfigurations",
        "(J)[Landroid/content/res/Configuration;",
        am_get_size_configurations,
    );
    reg(
        vm,
        am,
        "nativeGetSizeAndUiModeConfigurations",
        "(J)[Landroid/content/res/Configuration;",
        am_get_size_and_ui_mode_configurations,
    );
    reg(vm, am, "nativeApplyStyle", "(JJIIJ[IJJ)V", am_apply_style);
    reg(
        vm,
        am,
        "nativeResolveAttrs",
        "(JJII[I[I[I[I)Z",
        am_resolve_attrs,
    );
    reg(
        vm,
        am,
        "nativeRetrieveAttributes",
        "(JJ[I[I[I)Z",
        am_retrieve_attributes,
    );
    // The real `AssetManager.<clinit>` wraps `Theme` handles in a
    // `NativeAllocationRegistry.createMalloced(..., nativeGetThemeFreeFunction())`.
    // ACOS themes are Rust handles owned by the engine's table; a null free
    // function is the honest "no native allocation to free" answer.
    reg(vm, am, "nativeGetThemeFreeFunction", "()J", am_theme_free_function);
    reg(vm, am, "nativeThemeCreate", "(J)J", am_theme_create);
    reg(
        vm,
        am,
        "nativeThemeApplyStyle",
        "(JJIZ)V",
        am_theme_apply_style,
    );
    reg(
        vm,
        am,
        "nativeThemeRebase",
        "(JJ[I[ZI)V",
        am_theme_rebase,
    );
    reg(vm, am, "nativeThemeCopy", "(JJJJ)V", am_theme_copy);
    reg(
        vm,
        am,
        "nativeThemeGetAttributeValue",
        "(JJILandroid/util/TypedValue;Z)I",
        am_theme_get_attribute_value,
    );
    reg(
        vm,
        am,
        "nativeThemeGetChangingConfigurations",
        "(J)I",
        am_theme_changing_configurations,
    );
    reg(
        vm,
        am,
        "nativeGetParentThemeIdentifier",
        "(JI)I",
        am_get_parent_theme_identifier,
    );
    reg(vm, am, "nativeAssetDestroy", "(J)V", am_asset_destroy);
    reg(vm, am, "nativeAssetReadChar", "(J)I", am_asset_read_char);
    reg(vm, am, "nativeAssetRead", "(J[BII)I", am_asset_read);
    reg(vm, am, "nativeAssetSeek", "(JJI)J", am_asset_seek);
    reg(vm, am, "nativeAssetGetLength", "(J)J", am_asset_get_length);
    reg(
        vm,
        am,
        "nativeAssetGetRemainingLength",
        "(J)J",
        am_asset_get_remaining_length,
    );

    reg(
        vm,
        apk,
        "nativeLoad",
        "(ILjava/lang/String;ILandroid/content/res/loader/AssetsProvider;)J",
        apk_load,
    );
    reg(
        vm,
        apk,
        "nativeLoadEmpty",
        "(ILandroid/content/res/loader/AssetsProvider;)J",
        apk_load_empty,
    );
    reg(vm, apk, "nativeDestroy", "(J)V", apk_destroy);
    reg(
        vm,
        apk,
        "nativeGetAssetPath",
        "(J)Ljava/lang/String;",
        apk_get_asset_path,
    );
    reg(
        vm,
        apk,
        "nativeGetDebugName",
        "(J)Ljava/lang/String;",
        apk_get_debug_name,
    );
    reg(vm, apk, "nativeGetStringBlock", "(J)J", apk_get_string_block);
    reg(vm, apk, "nativeIsUpToDate", "(J)Z", apk_is_up_to_date);
    reg(
        vm,
        apk,
        "nativeOpenXml",
        "(JLjava/lang/String;)J",
        apk_open_xml,
    );
    reg(
        vm,
        apk,
        "nativeDefinesOverlayable",
        "(J)Z",
        apk_defines_overlayable,
    );

    reg(vm, sb, "nativeCreate", "([BII)J", sb_create);
    reg(vm, sb, "nativeDestroy", "(J)V", sb_destroy);
    reg(vm, sb, "nativeGetSize", "(J)I", sb_get_size);
    reg(
        vm,
        sb,
        "nativeGetString",
        "(JI)Ljava/lang/String;",
        sb_get_string,
    );
    reg(vm, sb, "nativeGetStyle", "(JI)[I", sb_get_style);

    reg(vm, xb, "nativeCreate", "([BII)J", xb_create);
    reg(vm, xb, "nativeDestroy", "(J)V", xb_destroy);
    reg(vm, xb, "nativeGetStringBlock", "(J)J", xb_get_string_block);
    reg(vm, xb, "nativeCreateParseState", "(JI)J", xb_create_parse_state);
    reg(
        vm,
        xb,
        "nativeDestroyParseState",
        "(J)V",
        xb_destroy_parse_state,
    );
    reg(vm, xb, "nativeNext", "(J)I", xb_next);
    reg(vm, xb, "nativeGetNamespace", "(J)I", xb_get_namespace);
    reg(vm, xb, "nativeGetName", "(J)I", xb_get_name);
    reg(vm, xb, "nativeGetText", "(J)I", xb_get_text);
    reg(vm, xb, "nativeGetLineNumber", "(J)I", xb_get_line_number);
    reg(
        vm,
        xb,
        "nativeGetAttributeCount",
        "(J)I",
        xb_get_attribute_count,
    );
    reg(
        vm,
        xb,
        "nativeGetAttributeNamespace",
        "(JI)I",
        xb_get_attribute_namespace,
    );
    reg(
        vm,
        xb,
        "nativeGetAttributeName",
        "(JI)I",
        xb_get_attribute_name,
    );
    reg(
        vm,
        xb,
        "nativeGetAttributeResource",
        "(JI)I",
        xb_get_attribute_resource,
    );
    reg(
        vm,
        xb,
        "nativeGetAttributeDataType",
        "(JI)I",
        xb_get_attribute_data_type,
    );
    reg(
        vm,
        xb,
        "nativeGetAttributeData",
        "(JI)I",
        xb_get_attribute_data,
    );
    reg(
        vm,
        xb,
        "nativeGetAttributeStringValue",
        "(JI)I",
        xb_get_attribute_string_value,
    );
    reg(vm, xb, "nativeGetIdAttribute", "(J)I", xb_get_id_attribute);
    reg(
        vm,
        xb,
        "nativeGetClassAttribute",
        "(J)I",
        xb_get_class_attribute,
    );
    reg(
        vm,
        xb,
        "nativeGetStyleAttribute",
        "(J)I",
        xb_get_style_attribute,
    );
    reg(vm, xb, "nativeGetSourceResId", "(J)I", xb_get_source_res_id);
    reg(
        vm,
        xb,
        "nativeGetAttributeIndex",
        "(JLjava/lang/String;Ljava/lang/String;)I",
        xb_get_attribute_index,
    );

    Ok(())
}

fn reg(vm: &mut Vm, class: &str, name: &str, sig: &str, f: NativeFn) {
    vm.register_native(class, name, sig, f);
}

// ---------------------------------------------------------------------------
// AssetManager natives
// ---------------------------------------------------------------------------

fn am_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = REG.with(|r| {
        let mut reg = r.borrow_mut();
        let h = reg.alloc();
        reg.managers.insert(h, AssetManager::new());
        h
    });
    vm.ret_i64(ctx, h as i64);
    Ok(())
}

fn am_destroy(_vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = _vm.arg_i64(ctx, 0) as u64;
    REG.with(|r| {
        r.borrow_mut().managers.remove(&h);
    });
    Ok(())
}

fn am_set_apk_assets(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let arr = vm.arg_ref(ctx, 1);
    let n = vm.array_length(arr).unwrap_or(0);
    let mut ptrs = Vec::with_capacity(n);
    for i in 0..n {
        if let Ok(JavaValue::Object(obj)) = vm.get_array_element(arr, i) {
            if let Ok(JavaValue::Long(p)) = vm.get_field(obj, "mNativePtr") {
                ptrs.push(p as u64);
            }
        }
    }
    let boot = vm.vfs.boot_root().to_path_buf();
    REG.with(|r| {
        let mut reg = r.borrow_mut();
        let mut apks = Vec::with_capacity(ptrs.len());
        for p in &ptrs {
            let Some(entry) = reg.apk_assets.get(p) else {
                continue;
            };
            match &entry.kind {
                ApkKind::Path(path) => {
                    let host = map_path_roots(&boot, vm.vfs.root(), path);
                    if let Ok(a) = ApkAssets::open(&host) {
                        apks.push(a);
                    }
                }
                ApkKind::Empty => {
                    if let Ok(a) = ApkAssets::from_bytes(Vec::new()) {
                        apks.push(a);
                    }
                }
            }
        }
        if let Some(m) = reg.managers.get_mut(&mgr) {
            m.set_apk_assets(apks);
        }
    });
    Ok(())
}

fn am_set_configuration(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let locale = vm
        .string_to_rust(vm.arg_ref(ctx, 3))
        .unwrap_or_default();
    let args = ConfigurationNative {
        mcc: vm.arg_i32(ctx, 1),
        mnc: vm.arg_i32(ctx, 2),
        default_locale: locale.clone(),
        locales: if locale.is_empty() {
            Vec::new()
        } else {
            vec![locale]
        },
        orientation: vm.arg_i32(ctx, 4),
        touchscreen: vm.arg_i32(ctx, 5),
        density: vm.arg_i32(ctx, 6),
        keyboard: vm.arg_i32(ctx, 7),
        keyboard_hidden: vm.arg_i32(ctx, 8),
        navigation: vm.arg_i32(ctx, 9),
        screen_width: vm.arg_i32(ctx, 10),
        screen_height: vm.arg_i32(ctx, 11),
        smallest_screen_width_dp: vm.arg_i32(ctx, 12),
        screen_width_dp: vm.arg_i32(ctx, 13),
        screen_height_dp: vm.arg_i32(ctx, 14),
        screen_layout: vm.arg_i32(ctx, 15),
        ui_mode: vm.arg_i32(ctx, 16),
        color_mode: vm.arg_i32(ctx, 17),
        grammatical_gender: vm.arg_i32(ctx, 18),
        major_version: vm.arg_i32(ctx, 19),
        force_refresh: false,
    };
    REG.with(|r| {
        if let Some(m) = r.borrow_mut().managers.get_mut(&mgr) {
            m.set_configuration(&args);
        }
    });
    Ok(())
}

fn am_get_assigned_package_identifiers(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let items = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .map(|m| m.assigned_package_identifiers())
            .unwrap_or_default()
    });
    let sa = vm
        .new_instance("android.util.SparseArray")
        .map_err(|e| internal(e.to_string()))?;
    // Fill the SparseArray's backing store directly. Calling `put` would run
    // `GrowingArrayUtils.insert` -> `Array.newInstance`/`System.arraycopy` as
    // Java frames from inside this native; the interpreter has no handler in a
    // native frame, so an exception there unwinds past this native's frame and
    // invalidates `ctx`. Constructing the real `SparseArray` fields
    // (`mKeys`/`mValues`/`mSize`) is exact and cannot unwind.
    let n = items.len();
    let keys = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "I".to_string(),
        data: ArrayData::Int(items.iter().map(|(id, _)| *id as i32).collect()),
    }))?;
    vm.native_roots.push(keys);
    let values = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/lang/Object;".to_string(),
        data: ArrayData::Ref(vec![NULL; n]),
    }))?;
    vm.native_roots.push(values);
    for (i, (_, name)) in items.iter().enumerate() {
        let nh = vm.new_string(name)?;
        if let Some(Object::Array(ArrayObj {
            data: ArrayData::Ref(v),
            ..
        })) = vm.heap.get_mut(values)
        {
            v[i] = nh;
        }
    }
    let _ = vm.set_field(sa, "mKeys", JavaValue::Object(keys));
    let _ = vm.set_field(sa, "mValues", JavaValue::Object(values));
    let _ = vm.set_field(sa, "mSize", JavaValue::Int(n as i32));
    vm.native_roots.pop();
    vm.native_roots.pop();
    detach(vm, sa);
    vm.ret_ref(ctx, sa);
    Ok(())
}

fn am_contains_allocated_table(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let b = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .map(|m| m.contains_allocated_table())
            .unwrap_or(false)
    });
    vm.ret_bool(ctx, b);
    Ok(())
}

fn am_list(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let dir = vm
        .string_to_rust(vm.arg_ref(ctx, 1))
        .unwrap_or_default();
    let mut names: Vec<String> = Vec::new();
    REG.with(|r| {
        let reg = r.borrow();
        if let Some(m) = reg.managers.get(&mgr) {
            for src in m.sources() {
                if let Ok(list) = src.list(&dir) {
                    for n in list {
                        if !names.contains(&n) {
                            names.push(n);
                        }
                    }
                }
            }
        }
    });
    let vals: Vec<Option<String>> = names.into_iter().map(Some).collect();
    let h = string_array(vm, &vals)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn am_open_asset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let name = vm
        .string_to_rust(vm.arg_ref(ctx, 1))
        .unwrap_or_default();
    let mode = vm.arg_i32(ctx, 2);
    let h = REG.with(|r| {
        r.borrow_mut()
            .managers
            .get_mut(&mgr)
            .and_then(|m| m.open_asset(&name, mode).ok())
            .unwrap_or(0)
    });
    vm.ret_i64(ctx, h as i64);
    Ok(())
}

fn am_open_non_asset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let cookie = vm.arg_i32(ctx, 1);
    let name = vm
        .string_to_rust(vm.arg_ref(ctx, 2))
        .unwrap_or_default();
    let mode = vm.arg_i32(ctx, 3);
    let h = REG.with(|r| {
        r.borrow_mut()
            .managers
            .get_mut(&mgr)
            .and_then(|m| m.open_non_asset(cookie, &name, mode).ok())
            .unwrap_or(0)
    });
    vm.ret_i64(ctx, h as i64);
    Ok(())
}

fn am_open_xml_asset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let cookie = vm.arg_i32(ctx, 1);
    let name = vm
        .string_to_rust(vm.arg_ref(ctx, 2))
        .unwrap_or_default();
    let r = REG.with(|r| {
        let mut reg = r.borrow_mut();
        let block = match reg
            .managers
            .get_mut(&mgr)
            .and_then(|m| m.open_xml_asset(cookie, &name).ok())
        {
            Some(b) => b,
            None => return 0,
        };
        alloc_block(&mut reg, mgr, block)
    });
    vm.ret_i64(ctx, r as i64);
    Ok(())
}

fn am_get_resource_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let id = vm.arg_i32(ctx, 1) as u32;
    let density = vm.arg_u16(ctx, 2);
    let tv = vm.arg_ref(ctx, 3);
    let resolve = vm.arg_bool(ctx, 4);
    let res = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .and_then(|m| m.get_resource_value(id, density, resolve))
    });
    match res {
        Some((cookie, out)) => {
            write_typed_value(vm, tv, &out);
            vm.ret_i32(ctx, cookie);
        }
        None => vm.ret_i32(ctx, 0),
    }
    Ok(())
}

fn am_get_resource_bag_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let id = vm.arg_i32(ctx, 1) as u32;
    let bag_entry = vm.arg_i32(ctx, 2) as u32;
    let tv = vm.arg_ref(ctx, 3);
    let res = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .and_then(|m| m.get_resource_bag_value(id, bag_entry))
    });
    match res {
        Some((cookie, out)) => {
            write_typed_value(vm, tv, &out);
            vm.ret_i32(ctx, cookie);
        }
        None => vm.ret_i32(ctx, 0),
    }
    Ok(())
}

fn am_get_style_attributes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let id = vm.arg_i32(ctx, 1) as u32;
    let v = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .and_then(|m| m.get_style_attributes(id))
    });
    match v {
        Some(v) => {
            let h = int_array(vm, v)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn am_get_resource_string_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let id = vm.arg_i32(ctx, 1) as u32;
    let v = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .and_then(|m| m.get_resource_string_array(id))
    });
    match v {
        Some(v) => {
            let h = string_array(vm, &v)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn am_get_resource_string_array_info(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let id = vm.arg_i32(ctx, 1) as u32;
    let v = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .and_then(|m| m.get_resource_string_array_info(id))
    });
    match v {
        Some(v) => {
            let h = int_array(vm, v)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn am_get_resource_int_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let id = vm.arg_i32(ctx, 1) as u32;
    let v = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .and_then(|m| m.get_resource_int_array(id))
    });
    match v {
        Some(v) => {
            let h = int_array(vm, v)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn am_get_resource_array_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let id = vm.arg_i32(ctx, 1) as u32;
    let n = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .map(|m| m.get_resource_array_size(id))
            .unwrap_or(0)
    });
    vm.ret_i32(ctx, n);
    Ok(())
}

fn am_get_resource_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let id = vm.arg_i32(ctx, 1) as u32;
    let oh = vm.arg_ref(ctx, 2);
    let mut out = read_ints(vm, oh).unwrap_or_default();
    let n = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .map(|m| m.get_resource_array(id, &mut out))
            .unwrap_or(0)
    });
    write_ints(vm, oh, &out);
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

fn am_get_resource_identifier(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let name = vm
        .string_to_rust(vm.arg_ref(ctx, 1))
        .unwrap_or_default();
    let def_type = vm.string_to_rust(vm.arg_ref(ctx, 2));
    let def_pkg = vm.string_to_rust(vm.arg_ref(ctx, 3));
    let id = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .map(|m| m.get_resource_identifier(&name, def_type.as_deref(), def_pkg.as_deref()))
            .unwrap_or(0)
    });
    vm.ret_i32(ctx, id as i32);
    Ok(())
}

fn am_string_result(vm: &mut Vm, ctx: NativeCtx, val: Option<String>) -> VmResult<()> {
    match val {
        Some(s) => {
            let h = vm.new_string(&s)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn am_get_resource_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let id = vm.arg_i32(ctx, 1) as u32;
    let v = REG.with(|r| r.borrow().managers.get(&mgr).and_then(|m| m.get_resource_name(id)));
    am_string_result(vm, ctx, v)
}

fn am_get_resource_package_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let id = vm.arg_i32(ctx, 1) as u32;
    let v = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .and_then(|m| m.get_resource_package_name(id))
    });
    am_string_result(vm, ctx, v)
}

fn am_get_resource_type_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let id = vm.arg_i32(ctx, 1) as u32;
    let v = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .and_then(|m| m.get_resource_type_name(id))
    });
    am_string_result(vm, ctx, v)
}

fn am_get_resource_entry_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let id = vm.arg_i32(ctx, 1) as u32;
    let v = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .and_then(|m| m.get_resource_entry_name(id))
    });
    am_string_result(vm, ctx, v)
}

fn am_get_locales(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let locales = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .map(|m| m.get_locales())
            .unwrap_or_default()
    });
    let vals: Vec<Option<String>> = locales.into_iter().map(Some).collect();
    let h = string_array(vm, &vals)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn am_get_size_configurations(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let cfgs = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .map(|m| m.get_size_configurations())
            .unwrap_or_default()
    });
    let h = config_array(vm, &cfgs)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn am_get_size_and_ui_mode_configurations(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let cfgs = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .map(|m| m.get_size_and_ui_mode_configurations())
            .unwrap_or_default()
    });
    let h = config_array(vm, &cfgs)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn am_apply_style(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let theme = vm.arg_i64(ctx, 1) as u64;
    let def_attr = vm.arg_i32(ctx, 2) as u32;
    let def_res = vm.arg_i32(ctx, 3) as u32;
    let pref = vm.arg_i64(ctx, 4) as u64;
    let attrs = read_ints(vm, vm.arg_ref(ctx, 5)).unwrap_or_default();
    // `outValues`/`outIndices` are `TypedArray.mDataAddress`/
    // `mIndicesAddress` (`VMRuntime.addressOf` synthetic addresses), not heap
    // handles. Decode the aliased array so the output is written back into the
    // `int[]` the framework will read.
    let ov_h = super::native_mem::array_handle_from_address(vm.arg_i64(ctx, 6))
        .map(|h| h as Handle)
        .unwrap_or(NULL);
    let oi_h = super::native_mem::array_handle_from_address(vm.arg_i64(ctx, 7))
        .map(|h| h as Handle)
        .unwrap_or(NULL);
    let mut ov = read_ints(vm, ov_h).unwrap_or_default();
    let mut oi = read_ints(vm, oi_h).unwrap_or_default();
    REG.with(|r| {
        let reg = r.borrow();
        let parse = reg.parse_refs.get(&pref).map(|p| p.parse).unwrap_or(0);
        if let Some(m) = reg.managers.get(&mgr) {
            m.apply_style(theme, def_attr, def_res, parse, &attrs, &mut ov, &mut oi);
        }
    });
    write_ints(vm, ov_h, &ov);
    write_ints(vm, oi_h, &oi);
    Ok(())
}

fn am_resolve_attrs(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let theme = vm.arg_i64(ctx, 1) as u64;
    let def_attr = vm.arg_i32(ctx, 2) as u32;
    let def_res = vm.arg_i32(ctx, 3) as u32;
    let in_h = vm.arg_ref(ctx, 4);
    let in_values = if in_h == NULL {
        None
    } else {
        read_ints(vm, in_h)
    };
    let attrs = read_ints(vm, vm.arg_ref(ctx, 5)).unwrap_or_default();
    let ov_h = vm.arg_ref(ctx, 6);
    let oi_h = vm.arg_ref(ctx, 7);
    let mut ov = read_ints(vm, ov_h).unwrap_or_default();
    let mut oi = read_ints(vm, oi_h).unwrap_or_default();
    let ok = REG.with(|r| {
        r.borrow().managers.get(&mgr).map(|m| {
            m.resolve_attrs(
                theme,
                def_attr,
                def_res,
                in_values.as_deref(),
                &attrs,
                &mut ov,
                &mut oi,
            )
        })
        .unwrap_or(false)
    });
    write_ints(vm, ov_h, &ov);
    write_ints(vm, oi_h, &oi);
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn am_retrieve_attributes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let pref = vm.arg_i64(ctx, 1) as u64;
    let attrs = read_ints(vm, vm.arg_ref(ctx, 2)).unwrap_or_default();
    let ov_h = vm.arg_ref(ctx, 3);
    let oi_h = vm.arg_ref(ctx, 4);
    let mut ov = read_ints(vm, ov_h).unwrap_or_default();
    let mut oi = read_ints(vm, oi_h).unwrap_or_default();
    let ok = REG.with(|r| {
        let reg = r.borrow();
        let parse = reg.parse_refs.get(&pref).map(|p| p.parse).unwrap_or(0);
        reg.managers
            .get(&mgr)
            .map(|m| m.retrieve_attributes(parse, &attrs, &mut ov, &mut oi))
            .unwrap_or(false)
    });
    write_ints(vm, ov_h, &ov);
    write_ints(vm, oi_h, &oi);
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn am_theme_free_function(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn am_theme_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let h = REG.with(|r| {
        r.borrow_mut()
            .managers
            .get_mut(&mgr)
            .map(|m| m.theme_create())
            .unwrap_or(0)
    });
    vm.ret_i64(ctx, h as i64);
    Ok(())
}

fn am_theme_apply_style(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let theme = vm.arg_i64(ctx, 1) as u64;
    let res_id = vm.arg_i32(ctx, 2) as u32;
    let force = vm.arg_bool(ctx, 3);
    REG.with(|r| {
        if let Some(m) = r.borrow_mut().managers.get_mut(&mgr) {
            m.theme_apply_style(theme, res_id, force);
        }
    });
    Ok(())
}

fn am_theme_rebase(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let theme = vm.arg_i64(ctx, 1) as u64;
    let ids: Vec<u32> = read_ints(vm, vm.arg_ref(ctx, 2))
        .unwrap_or_default()
        .into_iter()
        .map(|v| v as u32)
        .collect();
    let force = read_bools(vm, vm.arg_ref(ctx, 3)).unwrap_or_default();
    REG.with(|r| {
        if let Some(m) = r.borrow_mut().managers.get_mut(&mgr) {
            m.theme_rebase(theme, &ids, &force);
        }
    });
    Ok(())
}

fn am_theme_copy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let dst_mgr = vm.arg_i64(ctx, 0) as u64;
    let dst = vm.arg_i64(ctx, 1) as u64;
    let src_mgr = vm.arg_i64(ctx, 2) as u64;
    let src = vm.arg_i64(ctx, 3) as u64;
    REG.with(|r| {
        let mut reg = r.borrow_mut();
        if dst_mgr == src_mgr {
            if let Some(m) = reg.managers.get_mut(&dst_mgr) {
                m.theme_copy(dst, src);
            }
        } else {
            let src_holder = reg.managers.remove(&src_mgr);
            if let Some(src_holder) = src_holder.as_ref() {
                if let Some(dst_m) = reg.managers.get_mut(&dst_mgr) {
                    dst_m.import_theme_from(dst, src_holder, src);
                }
            }
            if let Some(src_holder) = src_holder {
                reg.managers.insert(src_mgr, src_holder);
            }
        }
    });
    Ok(())
}

fn am_theme_get_attribute_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let theme = vm.arg_i64(ctx, 1) as u64;
    let attr = vm.arg_i32(ctx, 2) as u32;
    let tv = vm.arg_ref(ctx, 3);
    let resolve = vm.arg_bool(ctx, 4);
    let res = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .and_then(|m| m.theme_get_attribute_value(theme, attr, resolve))
    });
    match res {
        Some((cookie, out)) => {
            write_typed_value(vm, tv, &out);
            vm.ret_i32(ctx, cookie);
        }
        None => vm.ret_i32(ctx, 0),
    }
    Ok(())
}

fn am_theme_changing_configurations(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = vm.arg_i64(ctx, 0);
    vm.ret_i32(ctx, 0);
    Ok(())
}

fn am_get_parent_theme_identifier(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let id = vm.arg_i32(ctx, 1) as u32;
    let v = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .map(|m| m.get_parent_theme_identifier(id))
            .unwrap_or(0)
    });
    vm.ret_i32(ctx, v as i32);
    Ok(())
}

fn am_asset_destroy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let handle = vm.arg_i64(ctx, 1) as u64;
    REG.with(|r| {
        if let Some(m) = r.borrow_mut().managers.get_mut(&mgr) {
            m.asset_destroy(handle);
        }
    });
    Ok(())
}

fn am_asset_read_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let handle = vm.arg_i64(ctx, 1) as u64;
    let n = REG.with(|r| {
        r.borrow_mut()
            .managers
            .get_mut(&mgr)
            .and_then(|m| m.asset_mut(handle))
            .map(|a| a.read_char())
            .unwrap_or(-1)
    });
    vm.ret_i32(ctx, n);
    Ok(())
}

fn am_asset_read(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let handle = vm.arg_i64(ctx, 1) as u64;
    let buf = vm.arg_ref(ctx, 2);
    let off = vm.arg_i32(ctx, 3) as usize;
    let len = vm.arg_i32(ctx, 4) as usize;
    let arr_len = vm.array_length(buf).unwrap_or(0);
    let mut tmp = vec![0u8; arr_len];
    let n = REG.with(|r| {
        r.borrow_mut()
            .managers
            .get_mut(&mgr)
            .and_then(|m| m.asset_mut(handle))
            .map(|a| a.read(&mut tmp, off, len))
            .unwrap_or(-1)
    });
    if n > 0 {
        write_bytes(vm, buf, off, &tmp[off..off + n as usize]);
    }
    vm.ret_i32(ctx, n);
    Ok(())
}

fn am_asset_seek(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let handle = vm.arg_i64(ctx, 1) as u64;
    let offset = vm.arg_i64(ctx, 2);
    let whence = vm.arg_i32(ctx, 3);
    let n = REG.with(|r| {
        r.borrow_mut()
            .managers
            .get_mut(&mgr)
            .and_then(|m| m.asset_mut(handle))
            .map(|a| a.seek(offset, whence))
            .unwrap_or(-1)
    });
    vm.ret_i64(ctx, n);
    Ok(())
}

fn am_asset_get_length(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let handle = vm.arg_i64(ctx, 1) as u64;
    let n = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .and_then(|m| m.asset(handle))
            .map(|a| a.length())
            .unwrap_or(0)
    });
    vm.ret_i64(ctx, n as i64);
    Ok(())
}

fn am_asset_get_remaining_length(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mgr = vm.arg_i64(ctx, 0) as u64;
    let handle = vm.arg_i64(ctx, 1) as u64;
    let n = REG.with(|r| {
        r.borrow()
            .managers
            .get(&mgr)
            .and_then(|m| m.asset(handle))
            .map(|a| a.remaining_length())
            .unwrap_or(0)
    });
    vm.ret_i64(ctx, n as i64);
    Ok(())
}

// ---------------------------------------------------------------------------
// ApkAssets natives
// ---------------------------------------------------------------------------

fn apk_load(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = vm
        .string_to_rust(vm.arg_ref(ctx, 1))
        .unwrap_or_default();
    let host = map_path_roots(vm.vfs.boot_root(), vm.vfs.root(), &path);
    let handle = match ApkAssets::open(&host) {
        Ok(a) => REG.with(|r| {
            let mut reg = r.borrow_mut();
            let h = reg.alloc();
            reg.apk_assets.insert(
                h,
                ApkEntry {
                    assets: a,
                    kind: ApkKind::Path(path),
                },
            );
            h
        }),
        Err(_) => 0,
    };
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn apk_load_empty(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = vm.arg_i32(ctx, 0);
    let handle = match ApkAssets::from_bytes(Vec::new()) {
        Ok(a) => REG.with(|r| {
            let mut reg = r.borrow_mut();
            let h = reg.alloc();
            reg.apk_assets.insert(
                h,
                ApkEntry {
                    assets: a,
                    kind: ApkKind::Empty,
                },
            );
            h
        }),
        Err(_) => 0,
    };
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn apk_destroy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    REG.with(|r| {
        r.borrow_mut().apk_assets.remove(&h);
    });
    Ok(())
}

fn apk_get_asset_path(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    let v = REG.with(|r| {
        r.borrow()
            .apk_assets
            .get(&h)
            .map(|e| e.assets.path().to_string())
    });
    am_string_result(vm, ctx, v)
}

fn apk_get_debug_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    let v = REG.with(|r| {
        r.borrow()
            .apk_assets
            .get(&h)
            .map(|e| e.assets.debug_name().to_string())
    });
    am_string_result(vm, ctx, v)
}

fn apk_get_string_block(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    let sh = REG.with(|r| {
        let mut reg = r.borrow_mut();
        let sb = reg.apk_assets.get(&h).and_then(string_block_from_apk);
        match sb {
            Some(sb) => {
                let sh = reg.alloc();
                reg.string_blocks.insert(sh, sb);
                sh
            }
            None => 0,
        }
    });
    vm.ret_i64(ctx, sh as i64);
    Ok(())
}

fn apk_is_up_to_date(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    let b = REG.with(|r| {
        r.borrow()
            .apk_assets
            .get(&h)
            .map(|e| e.assets.is_up_to_date())
            .unwrap_or(false)
    });
    vm.ret_bool(ctx, b);
    Ok(())
}

fn apk_open_xml(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    let name = vm
        .string_to_rust(vm.arg_ref(ctx, 1))
        .unwrap_or_default();
    let r = REG.with(|r| {
        let mut reg = r.borrow_mut();
        let bytes = match reg
            .apk_assets
            .get(&h)
            .and_then(|e| e.assets.read_entry(&name).ok())
        {
            Some(b) => b,
            None => return 0,
        };
        let block = match XmlBlock::from_bytes(&bytes, 0, bytes.len()) {
            Ok(b) => b,
            Err(_) => return 0,
        };
        let mgr = reg.orphan_manager();
        let engine_block = match reg.managers.get_mut(&mgr).map(|m| m.create_xml_block(block)) {
            Some(b) => b,
            None => return 0,
        };
        alloc_block(&mut reg, mgr, engine_block)
    });
    vm.ret_i64(ctx, r as i64);
    Ok(())
}

fn apk_defines_overlayable(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = vm.arg_i64(ctx, 0);
    vm.ret_bool(ctx, false);
    Ok(())
}

// ---------------------------------------------------------------------------
// StringBlock natives
// ---------------------------------------------------------------------------

fn sb_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let buf = vm.arg_ref(ctx, 0);
    let off = vm.arg_i32(ctx, 1) as usize;
    let size = vm.arg_i32(ctx, 2) as usize;
    let bytes = byte_span(vm, buf, off, size).unwrap_or_default();
    let h = match StringBlock::from_bytes(&bytes, 0, bytes.len()) {
        Ok(sb) => REG.with(|r| {
            let mut reg = r.borrow_mut();
            let h = reg.alloc();
            reg.string_blocks.insert(h, sb);
            h
        }),
        Err(_) => 0,
    };
    vm.ret_i64(ctx, h as i64);
    Ok(())
}

fn sb_destroy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    REG.with(|r| {
        r.borrow_mut().string_blocks.remove(&h);
    });
    Ok(())
}

fn sb_get_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    let n = REG.with(|r| {
        r.borrow()
            .string_blocks
            .get(&h)
            .map(|sb| sb.len() as i32)
            .unwrap_or(0)
    });
    vm.ret_i32(ctx, n);
    Ok(())
}

fn sb_get_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    let idx = vm.arg_i32(ctx, 1);
    let v = REG.with(|r| {
        r.borrow()
            .string_blocks
            .get(&h)
            .and_then(|sb| sb.get(idx))
            .map(|s| s.to_string())
    });
    am_string_result(vm, ctx, v)
}

fn sb_get_style(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    let idx = vm.arg_i32(ctx, 1);
    let spans: Vec<(i32, i32, i32)> = REG.with(|r| {
        r.borrow()
            .string_blocks
            .get(&h)
            .map(|sb| {
                sb.get_style(idx)
                    .iter()
                    .map(|s| (s.name as i32, s.first_char as i32, s.last_char as i32))
                    .collect()
            })
            .unwrap_or_default()
    });
    let mut vals = Vec::with_capacity(spans.len() * 3);
    for (a, b, c) in spans {
        vals.push(a);
        vals.push(b);
        vals.push(c);
    }
    let arr = int_array(vm, vals)?;
    vm.ret_ref(ctx, arr);
    Ok(())
}

// ---------------------------------------------------------------------------
// XmlBlock natives
// ---------------------------------------------------------------------------

fn xb_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let buf = vm.arg_ref(ctx, 0);
    let off = vm.arg_i32(ctx, 1) as usize;
    let size = vm.arg_i32(ctx, 2) as usize;
    let bytes = byte_span(vm, buf, off, size).unwrap_or_default();
    let r = REG.with(|r| {
        let mut reg = r.borrow_mut();
        let block = match XmlBlock::from_bytes(&bytes, 0, bytes.len()) {
            Ok(b) => b,
            Err(_) => return 0,
        };
        let mgr = reg.orphan_manager();
        let engine_block = match reg.managers.get_mut(&mgr).map(|m| m.create_xml_block(block)) {
            Some(b) => b,
            None => return 0,
        };
        alloc_block(&mut reg, mgr, engine_block)
    });
    vm.ret_i64(ctx, r as i64);
    Ok(())
}

fn xb_destroy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let block_ref = vm.arg_i64(ctx, 0) as u64;
    REG.with(|r| {
        let mut reg = r.borrow_mut();
        if let Some(b) = reg.block_refs.remove(&block_ref) {
            if let Some(m) = reg.managers.get_mut(&b.manager) {
                m.destroy_xml_block(b.block);
            }
        }
    });
    Ok(())
}

fn xb_get_string_block(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let block_ref = vm.arg_i64(ctx, 0) as u64;
    let sh = REG.with(|r| {
        let sb = {
            let reg = r.borrow();
            reg.block_refs
                .get(&block_ref)
                .and_then(|b| reg.managers.get(&b.manager)?.xml_block(b.block))
                .map(|blk| blk.doc.string_block.clone())
        };
        match sb {
            Some(sb) => {
                let mut reg = r.borrow_mut();
                let sh = reg.alloc();
                reg.string_blocks.insert(sh, sb);
                sh
            }
            None => 0,
        }
    });
    vm.ret_i64(ctx, sh as i64);
    Ok(())
}

fn xb_create_parse_state(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let block_ref = vm.arg_i64(ctx, 0) as u64;
    let res_id = vm.arg_i32(ctx, 1);
    let pref = REG.with(|r| {
        let mut reg = r.borrow_mut();
        let (mgr, block) = match reg.block_refs.get(&block_ref) {
            Some(b) => (b.manager, b.block),
            None => return 0,
        };
        let parse = match reg
            .managers
            .get_mut(&mgr)
            .and_then(|m| m.create_parse_state(block, res_id).ok())
        {
            Some(p) => p,
            None => return 0,
        };
        let pref = reg.alloc();
        reg.parse_refs.insert(
            pref,
            ParseRef {
                manager: mgr,
                block,
                parse,
            },
        );
        pref
    });
    vm.ret_i64(ctx, pref as i64);
    Ok(())
}

fn xb_destroy_parse_state(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let pref = vm.arg_i64(ctx, 0) as u64;
    REG.with(|r| {
        let mut reg = r.borrow_mut();
        if let Some(p) = reg.parse_refs.remove(&pref) {
            if let Some(m) = reg.managers.get_mut(&p.manager) {
                m.destroy_parse_state(p.parse);
            }
        }
    });
    Ok(())
}

fn xb_next(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let pref = vm.arg_i64(ctx, 0) as u64;
    let code = REG.with(|r| {
        let mut reg = r.borrow_mut();
        let (mgr, parse) = match reg.parse_refs.get(&pref) {
            Some(p) => (p.manager, p.parse),
            None => return -1,
        };
        reg.managers
            .get_mut(&mgr)
            .and_then(|m| m.parse_next(parse))
            .unwrap_or(-1)
    });
    vm.ret_i32(ctx, code);
    Ok(())
}

fn xb_query(
    vm: &mut Vm,
    ctx: NativeCtx,
    f: impl FnOnce(&XmlParserState, &XmlDocument) -> i32,
) -> VmResult<()> {
    let pref = vm.arg_i64(ctx, 0) as u64;
    let v = with_parse(pref, f).unwrap_or(-1);
    vm.ret_i32(ctx, v);
    Ok(())
}

fn xb_get_namespace(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    xb_query(vm, ctx, |s, d| s.get_namespace(d))
}

fn xb_get_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    xb_query(vm, ctx, |s, d| s.get_name(d))
}

fn xb_get_text(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    xb_query(vm, ctx, |s, d| s.get_text(d))
}

fn xb_get_line_number(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    xb_query(vm, ctx, |s, d| s.get_line_number(d))
}

fn xb_get_attribute_count(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    xb_query(vm, ctx, |s, d| s.get_attribute_count(d))
}

fn xb_get_id_attribute(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    xb_query(vm, ctx, |s, d| s.get_id_attribute(d))
}

fn xb_get_class_attribute(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    xb_query(vm, ctx, |s, d| s.get_class_attribute(d))
}

fn xb_get_style_attribute(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    xb_query(vm, ctx, |s, d| s.get_style_attribute(d))
}

fn xb_get_source_res_id(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    xb_query(vm, ctx, |s, _| s.get_source_res_id())
}

fn xb_get_attribute_namespace(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let pref = vm.arg_i64(ctx, 0) as u64;
    let idx = vm.arg_i32(ctx, 1);
    let v = with_parse(pref, |s, d| s.get_attribute_namespace(d, idx)).unwrap_or(-1);
    vm.ret_i32(ctx, v);
    Ok(())
}

fn xb_get_attribute_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let pref = vm.arg_i64(ctx, 0) as u64;
    let idx = vm.arg_i32(ctx, 1);
    let v = with_parse(pref, |s, d| s.get_attribute_name(d, idx)).unwrap_or(-1);
    vm.ret_i32(ctx, v);
    Ok(())
}

fn xb_get_attribute_resource(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let pref = vm.arg_i64(ctx, 0) as u64;
    let idx = vm.arg_i32(ctx, 1);
    let v = with_parse(pref, |s, d| s.get_attribute_resource(d, idx)).unwrap_or(-1);
    vm.ret_i32(ctx, v);
    Ok(())
}

fn xb_get_attribute_data_type(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let pref = vm.arg_i64(ctx, 0) as u64;
    let idx = vm.arg_i32(ctx, 1);
    let v = with_parse(pref, |s, d| s.get_attribute_data_type(d, idx)).unwrap_or(-1);
    vm.ret_i32(ctx, v);
    Ok(())
}

fn xb_get_attribute_data(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let pref = vm.arg_i64(ctx, 0) as u64;
    let idx = vm.arg_i32(ctx, 1);
    let v = with_parse(pref, |s, d| s.get_attribute_data(d, idx)).unwrap_or(-1);
    vm.ret_i32(ctx, v);
    Ok(())
}

fn xb_get_attribute_string_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let pref = vm.arg_i64(ctx, 0) as u64;
    let idx = vm.arg_i32(ctx, 1);
    let v = with_parse(pref, |s, d| s.get_attribute_string_value(d, idx)).unwrap_or(-1);
    vm.ret_i32(ctx, v);
    Ok(())
}

fn xb_get_attribute_index(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let pref = vm.arg_i64(ctx, 0) as u64;
    let ns = vm.string_to_rust(vm.arg_ref(ctx, 1));
    let name = vm
        .string_to_rust(vm.arg_ref(ctx, 2))
        .unwrap_or_default();
    let ns_opt = ns.as_deref().filter(|s| !s.is_empty());
    let v = with_parse(pref, |s, d| s.get_attribute_index(d, ns_opt, &name)).unwrap_or(-1);
    vm.ret_i32(ctx, v);
    Ok(())
}

// ---------------------------------------------------------------------------
// NDK host substrate accessors
//
// The production NDK host (`acos-jni`) holds no unsafe state: it resolves a
// Java `AssetManager`/`Bitmap`/`Surface` to a native handle and calls these
// safe `pub(crate)` functions. `asset_*` handles are engine handles scoped to a
// manager; `ndk_substrate` wraps them in a process-wide id namespace.
// ---------------------------------------------------------------------------

/// Whether `manager` names a live framework `AssetManager`.
pub(crate) fn ndk_manager_exists(manager: u64) -> bool {
    REG.with(|r| r.borrow().managers.contains_key(&manager))
}

/// The `ResConfig` of `manager`, or the closest live manager's real config.
pub(crate) fn ndk_manager_config(manager: u64) -> Option<ResConfig> {
    REG.with(|r| {
        let reg = r.borrow();
        if let Some(m) = reg.managers.get(&manager) {
            return Some(m.config().clone());
        }
        reg.managers
            .values()
            .find(|m| m.contains_allocated_table())
            .or_else(|| reg.managers.values().next())
            .map(|m| m.config().clone())
    })
}

fn ndk_lookup_manager(
    reg: &mut Registry,
    manager: u64,
) -> Result<&mut AssetManager, String> {
    reg.managers
        .get_mut(&manager)
        .ok_or_else(|| format!("unknown AAssetManager handle {manager:#x}"))
}

/// `AAssetManager_open`: open `filename` against the manager's real APK sources.
pub(crate) fn ndk_asset_open(manager: u64, filename: &str, mode: i32) -> Result<u64, String> {
    REG.with(|r| {
        let mut reg = r.borrow_mut();
        let m = ndk_lookup_manager(&mut reg, manager)?;
        m.open_asset(filename, mode).map_err(|e| e.to_string())
    })
}

/// `AAsset_read`: read into `out` from the asset's current position.
pub(crate) fn ndk_asset_read(manager: u64, asset: u64, out: &mut [u8]) -> Result<usize, String> {
    REG.with(|r| {
        let mut reg = r.borrow_mut();
        let m = ndk_lookup_manager(&mut reg, manager)?;
        let h = m
            .asset_mut(asset)
            .ok_or_else(|| format!("unknown AAsset handle {asset:#x}"))?;
        let n = h.read(out, 0, out.len());
        if n < 0 {
            Err("AAsset_read: buffer too small for the requested length".into())
        } else {
            Ok(n as usize)
        }
    })
}

/// `AAsset_seek`: move the read cursor (`whence` 0/1/2), returning the offset.
pub(crate) fn ndk_asset_seek(
    manager: u64,
    asset: u64,
    offset: i64,
    whence: i32,
) -> Result<i64, String> {
    REG.with(|r| {
        let mut reg = r.borrow_mut();
        let m = ndk_lookup_manager(&mut reg, manager)?;
        let h = m
            .asset_mut(asset)
            .ok_or_else(|| format!("unknown AAsset handle {asset:#x}"))?;
        Ok(h.seek(offset, whence))
    })
}

/// `AAsset_getLength`: the full asset length.
pub(crate) fn ndk_asset_length(manager: u64, asset: u64) -> Result<u64, String> {
    REG.with(|r| {
        let reg = r.borrow();
        let m = reg
            .managers
            .get(&manager)
            .ok_or_else(|| format!("unknown AAssetManager handle {manager:#x}"))?;
        m.asset(asset)
            .map(|h| h.length())
            .ok_or_else(|| format!("unknown AAsset handle {asset:#x}"))
    })
}

/// `AAsset_getRemainingLength`: bytes left from the current position.
pub(crate) fn ndk_asset_remaining(manager: u64, asset: u64) -> Result<u64, String> {
    REG.with(|r| {
        let reg = r.borrow();
        let m = reg
            .managers
            .get(&manager)
            .ok_or_else(|| format!("unknown AAssetManager handle {manager:#x}"))?;
        m.asset(asset)
            .map(|h| h.remaining_length())
            .ok_or_else(|| format!("unknown AAsset handle {asset:#x}"))
    })
}

/// `AAsset_getBuffer`: the whole entry, valid until `AAsset_close`.
///
/// The asset payload is never mutated after creation (reads only move the
/// cursor), so the returned address stays valid for the handle's lifetime.
pub(crate) fn ndk_asset_buffer(manager: u64, asset: u64) -> Result<*const u8, String> {
    REG.with(|r| {
        let reg = r.borrow();
        let m = reg
            .managers
            .get(&manager)
            .ok_or_else(|| format!("unknown AAssetManager handle {manager:#x}"))?;
        m.asset(asset)
            .map(|h| h.data.as_ptr())
            .ok_or_else(|| format!("unknown AAsset handle {asset:#x}"))
    })
}

/// `AAsset_isAllocated` — always true for an open handle in ACOS.
pub(crate) fn ndk_asset_is_allocated(manager: u64, asset: u64) -> bool {
    REG.with(|r| {
        r.borrow()
            .managers
            .get(&manager)
            .is_some_and(|m| m.asset(asset).is_some())
    })
}

/// `AAsset_close`: destroy the engine asset handle.
pub(crate) fn ndk_asset_close(manager: u64, asset: u64) {
    REG.with(|r| {
        if let Some(m) = r.borrow_mut().managers.get_mut(&manager) {
            m.asset_destroy(asset);
        }
    });
}

/// `AAssetManager_openDir`: allocate a cursor over the direct children of `dir`.
pub(crate) fn ndk_asset_dir(manager: u64, dir: &str) -> Result<Vec<String>, String> {
    REG.with(|r| {
        let reg = r.borrow();
        let m = reg
            .managers
            .get(&manager)
            .ok_or_else(|| format!("unknown AAssetManager handle {manager:#x}"))?;
        let norm = dir.trim_matches('/');
        let prefix = if norm.is_empty() {
            String::new()
        } else {
            format!("{norm}/")
        };
        let mut names = Vec::new();
        for src in m.sources() {
            if let Ok(list) = src.list(norm) {
                for name in list {
                    let rest = name.strip_prefix(&prefix).unwrap_or(&name);
                    if !rest.is_empty() && !rest.contains('/') {
                        names.push(rest.to_string());
                    }
                }
            }
        }
        names.sort();
        names.dedup();
        if names.is_empty() {
            Err(format!("asset dir not found in APK sources: {dir}"))
        } else {
            Ok(names)
        }
    })
}

/// Allocate a registry-local cursor id for an `AAssetDir` list.
pub(crate) fn ndk_dir_store(names: Vec<String>) -> u64 {
    REG.with(|r| {
        let mut reg = r.borrow_mut();
        let id = reg.alloc();
        reg.dirs.insert(id, (names, 0));
        id
    })
}

/// `AAssetDir_getNextFileName` — the next unread captured name, else `None`.
pub(crate) fn ndk_dir_next(id: u64) -> Option<String> {
    REG.with(|r| {
        let mut reg = r.borrow_mut();
        let (names, next) = reg.dirs.get_mut(&id)?;
        let name = names.get(*next).cloned()?;
        *next += 1;
        Some(name)
    })
}

/// `AAssetDir_rewind`.
pub(crate) fn ndk_dir_rewind(id: u64) {
    REG.with(|r| {
        if let Some((_, next)) = r.borrow_mut().dirs.get_mut(&id) {
            *next = 0;
        }
    });
}

/// `AAssetDir_close`.
pub(crate) fn ndk_dir_close(id: u64) {
    REG.with(|r| {
        r.borrow_mut().dirs.remove(&id);
    });
}

/// Test-only: install a real `AssetManager` into the registry so
/// `ndk_substrate` can be exercised without the framework Java layer.
#[cfg(test)]
pub(crate) fn test_install_manager(ptr: u64, manager: AssetManager) {
    REG.with(|r| {
        r.borrow_mut().managers.insert(ptr, manager);
    });
}
