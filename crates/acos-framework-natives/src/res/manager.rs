//! `AssetManager`: layered [`ApkAssets`] sources, the current resource
//! configuration, asset/XML handle tables, and the resource lookup entry
//! points behind the `nativeGet*` methods.
//!
//! Reference resolution (`@resid` chains and `?attr` against a theme) is
//! implemented here because it spans sources.

use std::collections::HashMap;

use crate::res::arsc::ResolvedEntry;
use crate::res::assets::{ApkAssets, AssetHandle, AssetSource};
use crate::res::config::ResConfig;
use crate::res::error::{ResError, Result, RES_BAD_HANDLE};
use crate::res::theme::Theme;
use crate::res::value::{ResValue, TypedValueOut, STYLE_NUM_ENTRIES};
use crate::res::xmlblock::{XmlBlock, XmlParserState};

/// Maximum reference-chasing depth (`ResTable::MAX_REFERENCED_RES`-ish).
pub const MAX_REFERENCE_DEPTH: usize = 20;

/// The native `AssetManager.nativeSetConfiguration` argument set.
#[derive(Debug, Clone)]
pub struct ConfigurationNative {
    /// Mobile country code.
    pub mcc: i32,
    /// Mobile network code.
    pub mnc: i32,
    /// Default locale (BCP-47).
    pub default_locale: String,
    /// Locale list (BCP-47).
    pub locales: Vec<String>,
    /// Orientation.
    pub orientation: i32,
    /// Touchscreen.
    pub touchscreen: i32,
    /// Density dpi.
    pub density: i32,
    /// Keyboard.
    pub keyboard: i32,
    /// Keyboard hidden state.
    pub keyboard_hidden: i32,
    /// Navigation.
    pub navigation: i32,
    /// Screen width in px.
    pub screen_width: i32,
    /// Screen height in px.
    pub screen_height: i32,
    /// Smallest screen width in dp.
    pub smallest_screen_width_dp: i32,
    /// Screen width in dp.
    pub screen_width_dp: i32,
    /// Screen height in dp.
    pub screen_height_dp: i32,
    /// `screenLayout` byte.
    pub screen_layout: i32,
    /// `uiMode` byte.
    pub ui_mode: i32,
    /// `colorMode` byte.
    pub color_mode: i32,
    /// Grammatical gender.
    pub grammatical_gender: i32,
    /// Major platform version.
    pub major_version: i32,
    /// Force refresh.
    pub force_refresh: bool,
}

impl ConfigurationNative {
    /// Build a [`ResConfig`] from the native arguments.
    #[allow(clippy::field_reassign_with_default)]
    pub fn to_res_config(&self) -> ResConfig {
        let mut c = ResConfig::default();
        c.mcc = self.mcc as u16;
        c.mnc = self.mnc as u16;
        if let Some(loc) = parse_bcp47(&self.default_locale) {
            c.language = loc.0;
            c.country = loc.1;
        }
        c.orientation = self.orientation as u8;
        c.touchscreen = self.touchscreen as u8;
        c.density = self.density as u16;
        c.keyboard = self.keyboard as u8;
        c.navigation = self.navigation as u8;
        c.screen_width = self.screen_width as u16;
        c.screen_height = self.screen_height as u16;
        c.smallest_screen_width_dp = self.smallest_screen_width_dp.max(0) as u16;
        c.screen_width_dp = self.screen_width_dp.max(0) as u16;
        c.screen_height_dp = self.screen_height_dp.max(0) as u16;
        c.screen_layout = self.screen_layout as u8;
        c.ui_mode = self.ui_mode as u8;
        c.color_mode = self.color_mode as u8;
        c.grammatical_inflection = self.grammatical_gender.max(0) as u8;
        c.sdk_version = self.major_version.max(0) as u16;
        let input = self.keyboard_hidden as u8;
        c.input_flags = input;
        c
    }
}

fn parse_bcp47(locale: &str) -> Option<([u8; 2], [u8; 2])> {
    let mut parts = locale.split(['-', '_']);
    let lang = parts.next()?;
    let mut language = [0u8; 2];
    let mut it = lang.bytes();
    language[0] = it.next()?;
    language[1] = it.next().unwrap_or(0);
    let mut country = [0u8; 2];
    for p in parts {
        if p.len() == 2 && p.bytes().all(|b| b.is_ascii_uppercase()) {
            let mut c = p.bytes();
            country[0] = c.next().unwrap_or(0);
            country[1] = c.next().unwrap_or(0);
            break;
        }
    }
    Some((language, country))
}

/// The `android:targetPackage` declared in an overlay APK's manifest.
///
/// `None` means the overlay did not declare one (or the manifest could not be
/// read), in which case its entries are matched against any base package.
fn overlay_target_package(apk: &ApkAssets) -> Option<String> {
    let data = apk.read_entry("AndroidManifest.xml").ok()?;
    let doc = crate::res::xmlblock::parse_axml(&data).ok()?;
    for event in &doc.events {
        let crate::res::xmlblock::XmlEvent::StartTag(tag) = event else {
            continue;
        };
        if doc.string_block.get(tag.name_index) != Some("overlay") {
            continue;
        }
        for attr in &tag.attributes {
            if doc.string_block.get(attr.name_index) == Some("targetPackage") {
                return doc
                    .string_block
                    .get(attr.raw_value_index)
                    .map(str::to_string);
            }
        }
    }
    None
}

/// A resolved lookup hit.
#[derive(Debug, Clone)]
pub struct Lookup {
    /// Index of the source in `sources`.
    pub source_index: usize,
    /// The resolved entry.
    pub entry: ResolvedEntry,
}

/// A static resource overlay (RRO) applied over the base sources.
///
/// A static overlay cannot be enabled or disabled at runtime, so it is applied
/// unconditionally when its APK is loaded. `map` translates the overlay's own
/// package-local resource ids to the *target* ids they override.
///
/// The overlay APK is deliberately kept out of `sources`: the Java
/// `AssetManager` caches one `StringBlock` per `ApkAssets` it passes to
/// `nativeSetApkAssets`, so an extra native source would desynchronise that
/// array (`ArrayIndexOutOfBoundsException` in `getPooledStringForCookie`).
#[derive(Debug)]
pub struct Overlay {
    /// The overlay APK.
    pub apk: ApkAssets,
    /// Target resource id -> overlay-local resource id.
    pub map: HashMap<u32, u32>,
}

/// The Android `AssetManager` engine.
#[derive(Debug, Default)]
pub struct AssetManager {
    sources: Vec<ApkAssets>,
    config: ResConfig,
    locales: Vec<String>,
    assets: HashMap<u64, AssetHandle>,
    next_asset: u64,
    blocks: HashMap<u64, XmlBlock>,
    next_block: u64,
    parse_states: HashMap<u64, XmlParserState>,
    next_parse_state: u64,
    themes: HashMap<u64, Theme>,
    next_theme: u64,
    default_theme: u64,
    cookie_to_source: HashMap<i32, usize>,
    /// Applied static overlays, in application (priority) order.
    overlays: Vec<Overlay>,
}

impl AssetManager {
    /// Create an empty manager.
    pub fn new() -> Self {
        AssetManager::default()
    }

    /// Append an APK source and return its asset cookie.
    pub fn add_apk_assets(&mut self, apk: ApkAssets) -> i32 {
        let cookie = self.sources.len() as i32 + 1;
        self.cookie_to_source.insert(cookie, self.sources.len());
        self.sources.push(apk);
        cookie
    }

    /// Replace all sources (`nativeSetApkAssets`).
    pub fn set_apk_assets(&mut self, apks: Vec<ApkAssets>) {
        self.sources.clear();
        self.cookie_to_source.clear();
        self.overlays.clear();
        for apk in apks {
            self.add_apk_assets(apk);
        }
    }

    /// Apply a static RRO APK over the currently loaded base sources.
    ///
    /// Each overlay entry is matched to the base by `(target package, type,
    /// key)` — the same name mapping `idmap2` produces for a simple static
    /// overlay. The overlay is held out of `sources` and only its simple
    /// scalar values (bool / int / colour / dimension / fraction) are applied;
    /// a string or `@resid` would need the overlay's own string pool or
    /// package id, neither of which the Java `AssetManager` can address for a
    /// source it never saw. Returns the number of overridden resources.
    pub fn apply_overlay(&mut self, apk: ApkAssets) -> usize {
        let target_pkg = overlay_target_package(&apk);
        let (Some(table), Some(bytes)) = (apk.resource_table(), apk.resource_bytes()) else {
            return 0;
        };
        let entries = table.entries(bytes);
        let mut map = HashMap::new();
        for (local_id, type_name, key) in entries {
            let name = format!("{type_name}/{key}");
            if let Some(target_id) = self.find_target_id(&name, target_pkg.as_deref()) {
                map.insert(target_id, local_id);
            }
        }
        let count = map.len();
        self.overlays.push(Overlay { apk, map });
        count
    }

    /// The id of a resource in a base source, by name.
    fn find_target_id(&self, name: &str, target_pkg: Option<&str>) -> Option<u32> {
        for src in &self.sources {
            let (Some(table), Some(bytes)) = (src.resource_table(), src.resource_bytes()) else {
                continue;
            };
            if let Some(id) = table.identifier(bytes, name, None, target_pkg) {
                return Some(id);
            }
        }
        None
    }

    /// Resolve `id` against a base source, with the source index.
    fn lookup_base(&self, id: u32, requested: &ResConfig) -> Option<(usize, ResolvedEntry)> {
        for (i, src) in self.sources.iter().enumerate() {
            let (Some(table), Some(bytes)) = (src.resource_table(), src.resource_bytes()) else {
                continue;
            };
            if let Some(entry) = table.resolve(bytes, id, requested) {
                return Some((i, entry));
            }
        }
        None
    }

    /// The loaded sources.
    pub fn sources(&self) -> &[ApkAssets] {
        &self.sources
    }

    /// Source for a cookie.
    pub fn source_for_cookie(&self, cookie: i32) -> Option<&ApkAssets> {
        self.cookie_to_source
            .get(&cookie)
            .and_then(|i| self.sources.get(*i))
    }

    /// The current configuration.
    pub fn config(&self) -> &ResConfig {
        &self.config
    }

    /// The current density.
    pub fn density(&self) -> u16 {
        self.config.density
    }

    /// Apply a `nativeSetConfiguration` argument set.
    pub fn set_configuration(&mut self, args: &ConfigurationNative) {
        self.config = args.to_res_config();
        self.locales = args.locales.clone();
        if self.locales.is_empty() && !args.default_locale.is_empty() {
            self.locales.push(args.default_locale.clone());
        }
    }

    /// The locales as BCP-47 strings.
    pub fn locales(&self) -> &[String] {
        &self.locales
    }

    /// `nativeGetAssignedPackageIdentifiers`.
    pub fn assigned_package_identifiers(&self) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        for src in &self.sources {
            if let Some(table) = src.resource_table() {
                for pkg in &table.packages {
                    if !out.iter().any(|(id, _)| *id == pkg.id) {
                        out.push((pkg.id, pkg.name.clone()));
                    }
                }
            }
        }
        out
    }

    /// Resolve `id` in the current configuration.
    pub fn lookup(&self, id: u32) -> Option<Lookup> {
        self.lookup_with(id, &self.config)
    }

    /// Resolve `id` in an explicit configuration.
    pub fn lookup_with(&self, id: u32, requested: &ResConfig) -> Option<Lookup> {
        // Static overlays win over the base, highest priority last.
        for ov in self.overlays.iter().rev() {
            let Some(local_id) = ov.map.get(&id) else {
                continue;
            };
            let (Some(table), Some(bytes)) =
                (ov.apk.resource_table(), ov.apk.resource_bytes())
            else {
                continue;
            };
            let Some(mut entry) = table.resolve(bytes, *local_id, requested) else {
                continue;
            };
            // Skip values that would need the overlay's own string pool or
            // package id (see `apply_overlay`).
            if entry.value.data_type == crate::res::value::TYPE_STRING
                || entry.value.is_reference()
                || entry.bag.is_some()
            {
                continue;
            }
            // Present the target's identity so `get_resource_name` and the
            // `get_resource_{package,type,entry}_name` natives stay consistent
            // for an overridden id, and anchor the cookie on the base source
            // the Java `AssetManager` knows about.
            let base = self.lookup_base(id, requested);
            let (source_index, base_entry) = base.unwrap_or((0, entry.clone()));
            entry.package_name = base_entry.package_name;
            entry.type_name = base_entry.type_name;
            entry.key = base_entry.key;
            entry.type_id = base_entry.type_id;
            entry.entry_id = base_entry.entry_id;
            return Some(Lookup {
                source_index,
                entry,
            });
        }
        for (i, src) in self.sources.iter().enumerate() {
            let Some(table) = src.resource_table() else {
                continue;
            };
            if let Some(entry) = table.resolve(src.resource_bytes()?, id, requested) {
                return Some(Lookup {
                    source_index: i,
                    entry,
                });
            }
        }
        None
    }

    /// Resolve a reference chain to a final entry.
    ///
    /// Returns `(source_index, entry, final_id, cookie)`.
    pub fn resolve_references(
        &self,
        id: u32,
        resolve_refs: bool,
    ) -> Option<(usize, ResolvedEntry, u32)> {
        self.resolve_references_with(id, &self.config, resolve_refs)
    }

    /// Resolve a reference chain under an explicit configuration.
    fn resolve_references_with(
        &self,
        id: u32,
        requested: &ResConfig,
        resolve_refs: bool,
    ) -> Option<(usize, ResolvedEntry, u32)> {
        let mut cur = id;
        for _ in 0..MAX_REFERENCE_DEPTH {
            let hit = self.lookup_with(cur, requested)?;
            // `AssetManager2::GetResource(resid, may_be_bag=true)`: a complex
            // (bag/map) entry can't be represented as a `Res_value`, so AOSP
            // synthesises a `TYPE_REFERENCE` to the bag id. `ResolveReference`
            // stops there (`result->data == resolve_resid`), keeping the
            // reference. This is what makes `?attr/textAppearance*` a
            // `TYPE_REFERENCE` to the style bag, so
            // `TypedArray.getResourceId`/`Theme.resolveAttribute` hand the
            // framework the bag it must inflate.
            if hit.entry.bag.is_some() {
                let mut entry = hit.entry.clone();
                entry.value = ResValue::reference(cur);
                return Some((hit.source_index, entry, cur));
            }
            let value = hit.entry.value;
            match value.data_type {
                crate::res::value::TYPE_REFERENCE | crate::res::value::TYPE_DYNAMIC_REFERENCE
                    if resolve_refs =>
                {
                    if value.data == 0 {
                        // A `@null` entry is materialised as `TYPE_NULL`.
                        // Returning the raw `TYPE_REFERENCE` with `data=0`
                        // made `ResourcesImpl.loadDrawableForCookie` throw
                        // `NotFoundException: 108027e is not a Drawable`
                        // (`config_scrollbarTrackVertical` = `@null`).
                        let mut entry = hit.entry.clone();
                        entry.value = ResValue::NULL;
                        return Some((hit.source_index, entry, cur));
                    }
                    if value.data == cur {
                        // Self-referential entry: AOSP stops rather than
                        // looping (`result->data == resolve_resid`).
                        return Some((hit.source_index, hit.entry, cur));
                    }
                    cur = value.data;
                }
                crate::res::value::TYPE_ATTRIBUTE | crate::res::value::TYPE_DYNAMIC_ATTRIBUTE
                    if resolve_refs =>
                {
                    // `?attr` resolves against the default theme when present.
                    // The theme value may itself be a `@resid` (e.g.
                    // `colorControlHighlight` -> `@color/ripple_material_light`),
                    // so keep chasing the chain. Returning the raw reference
                    // made the framework throw `Can't convert to ComplexColor:
                    // type=0x1` while inflating `btn_default_material`.
                    if let Some(v) = self.theme_resolve_attr(self.default_theme, value.data) {
                        if v.is_reference() && v.data != 0 {
                            cur = v.data;
                            continue;
                        }
                        let mut entry = hit.entry.clone();
                        entry.value = if v.is_reference() {
                            // A null reference is materialised as TYPE_NULL.
                            ResValue::NULL
                        } else {
                            v
                        };
                        return Some((hit.source_index, entry, cur));
                    }
                    return Some((hit.source_index, hit.entry, cur));
                }
                _ => return Some((hit.source_index, hit.entry, cur)),
            }
        }
        None
    }

    #[allow(clippy::field_reassign_with_default)]
    pub(crate) fn build_typed_value(
        &self,
        source_index: usize,
        entry: &ResolvedEntry,
        requested_id: u32,
        final_id: u32,
        density_override: u16,
    ) -> Option<(i32, TypedValueOut)> {
        let src = self.sources.get(source_index)?;
        let cookie = source_index as i32 + 1;
        let mut out = TypedValueOut::default();
        out.data_type = entry.value.data_type;
        out.data = entry.value.data;
        out.asset_cookie = cookie;
        // `STYLE_RESOURCE_ID` is the resource the value was loaded from (the
        // resolved target of a `@resid`, not the requested attribute id).
        // `TypedArray.getResourceId` reads this slot.
        out.resource_id = final_id;
        out.source_resource_id = requested_id;
        out.changing_configurations = entry.changing_configurations as i32;
        out.density = if density_override != 0 {
            density_override
        } else {
            entry.config.density
        };
        if entry.value.data_type == crate::res::value::TYPE_STRING {
            if let Some(table) = src.resource_table() {
                out.string = table.global_string(src.resource_bytes()?, entry.value.data);
            }
        }
        Some((cookie, out))
    }

    /// The ARSC global string at `idx` in `sources[source_index]`.
    ///
    /// A `TYPE_STRING` value taken from a style/theme bag stores its `data` as
    /// an index into that APK's global string pool (identified to the framework
    /// by `assetCookie = source_index + 1`). XML-tag values keep cookie `-1`
    /// and read through the parser's own `StringBlock` instead.
    pub(crate) fn pooled_string(&self, source_index: usize, idx: u32) -> Option<String> {
        let src = self.sources.get(source_index)?;
        let table = src.resource_table()?;
        table.global_string(src.resource_bytes()?, idx)
    }

    /// `nativeGetResourceValue` (without reference resolution this returns the
    /// raw value).
    pub fn get_resource_value(
        &self,
        id: u32,
        density: u16,
        resolve_refs: bool,
    ) -> Option<(i32, TypedValueOut)> {
        let (source_index, entry, final_id) = self.resolve_references(id, resolve_refs)?;
        self.build_typed_value(source_index, &entry, id, final_id, density)
    }

    /// `nativeGetResourceBagValue`.
    pub fn get_resource_bag_value(
        &self,
        id: u32,
        bag_entry_id: u32,
    ) -> Option<(i32, TypedValueOut)> {
        let hit = self.lookup(id)?;
        let bag = hit.entry.bag.as_ref()?;
        let (_, value) = bag.entries.iter().find(|(n, _)| *n == bag_entry_id)?;
        let mut entry = hit.entry.clone();
        entry.value = *value;
        entry.changing_configurations = 0;
        self.build_typed_value(hit.source_index, &entry, id, id, 0)
    }

    /// `nativeGetResourceArray` — fills `out_values` with 7-int entries.
    pub fn get_resource_array(&self, id: u32, out_values: &mut [i32]) -> usize {
        let Some(hit) = self.lookup(id) else {
            return 0;
        };
        let Some(bag) = hit.entry.bag.as_ref() else {
            return 0;
        };
        let count = bag.entries.len().min(out_values.len() / STYLE_NUM_ENTRIES);
        let src = &self.sources[hit.source_index];
        for (i, (name, value)) in bag.entries.iter().take(count).enumerate() {
            let mut entry = hit.entry.clone();
            entry.value = *value;
            let mut tv = TypedValueOut {
                data_type: value.data_type,
                data: value.data,
                asset_cookie: hit.source_index as i32 + 1,
                resource_id: *name,
                changing_configurations: 0,
                density: entry.config.density,
                source_resource_id: 0,
                flags: 0,
                string: None,
            };
            if value.data_type == crate::res::value::TYPE_STRING {
                if let Some(table) = src.resource_table() {
                    if let Some(rb) = src.resource_bytes() {
                        tv.string = table.global_string(rb, value.data);
                    }
                }
            }
            tv.write_style(&mut out_values[i * STYLE_NUM_ENTRIES..]);
        }
        count
    }

    /// `nativeGetResourceArraySize`.
    pub fn get_resource_array_size(&self, id: u32) -> i32 {
        self.lookup(id)
            .and_then(|h| h.entry.bag)
            .map(|b| b.entries.len() as i32)
            .unwrap_or(0)
    }

    /// `nativeGetResourceStringArray`.
    pub fn get_resource_string_array(&self, id: u32) -> Option<Vec<Option<String>>> {
        let hit = self.lookup(id)?;
        let bag = hit.entry.bag?;
        let src = &self.sources[hit.source_index];
        let table = src.resource_table()?;
        let mut out = Vec::with_capacity(bag.entries.len());
        for (_, value) in &bag.entries {
            let resolved = self.resolve_value_to_string(src, table, *value, 0)?;
            out.push(resolved);
        }
        Some(out)
    }

    fn resolve_value_to_string(
        &self,
        src: &ApkAssets,
        table: &crate::res::arsc::ResTableIndex,
        value: ResValue,
        depth: usize,
    ) -> Option<Option<String>> {
        if depth > MAX_REFERENCE_DEPTH {
            return Some(None);
        }
        match value.data_type {
            crate::res::value::TYPE_STRING => {
                Some(table.global_string(src.resource_bytes()?, value.data))
            }
            crate::res::value::TYPE_REFERENCE | crate::res::value::TYPE_DYNAMIC_REFERENCE => {
                let hit = self.lookup(value.data)?;
                let s = &self.sources[hit.source_index];
                let t = s.resource_table()?;
                self.resolve_value_to_string(s, t, hit.entry.value, depth + 1)
            }
            _ => Some(None),
        }
    }

    /// `nativeGetResourceStringArrayInfo` — `(cookie, string_index)` pairs.
    pub fn get_resource_string_array_info(&self, id: u32) -> Option<Vec<i32>> {
        let hit = self.lookup(id)?;
        let bag = hit.entry.bag?;
        let src = &self.sources[hit.source_index];
        let table = src.resource_table()?;
        let mut out = Vec::with_capacity(bag.entries.len() * 2);
        for (_, value) in &bag.entries {
            let mut v = *value;
            let mut depth = 0;
            while v.is_reference() && depth < MAX_REFERENCE_DEPTH {
                match self.lookup(v.data) {
                    Some(h) => {
                        v = h.entry.value;
                        depth += 1;
                    }
                    None => break,
                }
            }
            let cookie = hit.source_index as i32 + 1;
            if v.data_type == crate::res::value::TYPE_STRING
                && table.global_string(src.resource_bytes()?, v.data).is_some()
            {
                out.push(cookie);
                out.push(v.data as i32);
            } else {
                out.push(-1);
                out.push(-1);
            }
        }
        Some(out)
    }

    /// `nativeGetResourceIntArray`.
    pub fn get_resource_int_array(&self, id: u32) -> Option<Vec<i32>> {
        let hit = self.lookup(id)?;
        let bag = hit.entry.bag?;
        Some(bag.entries.iter().map(|(_, v)| v.data as i32).collect())
    }

    /// `nativeGetStyleAttributes`.
    pub fn get_style_attributes(&self, id: u32) -> Option<Vec<i32>> {
        let hit = self.lookup(id)?;
        let bag = hit.entry.bag?;
        Some(bag.entries.iter().map(|(n, _)| *n as i32).collect())
    }

    /// `nativeGetResourceName` (`package:type/entry`).
    pub fn get_resource_name(&self, id: u32) -> Option<String> {
        let hit = self.lookup(id)?;
        Some(format!(
            "{}:{}/{}",
            hit.entry.package_name, hit.entry.type_name, hit.entry.key
        ))
    }

    /// `nativeGetResourcePackageName`.
    pub fn get_resource_package_name(&self, id: u32) -> Option<String> {
        self.lookup(id).map(|h| h.entry.package_name)
    }

    /// `nativeGetResourceTypeName`.
    pub fn get_resource_type_name(&self, id: u32) -> Option<String> {
        self.lookup(id).map(|h| h.entry.type_name)
    }

    /// `nativeGetResourceEntryName`.
    pub fn get_resource_entry_name(&self, id: u32) -> Option<String> {
        self.lookup(id).map(|h| h.entry.key)
    }

    /// `nativeGetResourceIdentifier`.
    pub fn get_resource_identifier(
        &self,
        name: &str,
        def_type: Option<&str>,
        def_package: Option<&str>,
    ) -> u32 {
        for src in &self.sources {
            if let Some(table) = src.resource_table() {
                if let Some(rb) = src.resource_bytes() {
                    if let Some(id) = table.identifier(rb, name, def_type, def_package) {
                        return id;
                    }
                }
            }
        }
        0
    }

    /// `nativeGetParentThemeIdentifier`.
    pub fn get_parent_theme_identifier(&self, id: u32) -> u32 {
        let Some(hit) = self.lookup(id) else {
            return 0;
        };
        let Some(bag) = hit.entry.bag.as_ref() else {
            return 0;
        };
        bag.parent
    }

    /// `nativeGetSizeConfigurations`.
    pub fn get_size_configurations(&self) -> Vec<ResConfig> {
        let mut out = Vec::new();
        for src in &self.sources {
            if let Some(table) = src.resource_table() {
                for c in table.size_configurations(src.resource_bytes().unwrap_or(&[])) {
                    if !out.contains(&c) {
                        out.push(c);
                    }
                }
            }
        }
        out
    }

    /// `nativeGetSizeAndUiModeConfigurations`.
    pub fn get_size_and_ui_mode_configurations(&self) -> Vec<ResConfig> {
        let mut out = self.get_size_configurations();
        for src in &self.sources {
            if let Some(table) = src.resource_table() {
                for c in table.all_configurations(src.resource_bytes().unwrap_or(&[])) {
                    if !out.contains(&c) {
                        out.push(c);
                    }
                }
            }
        }
        out
    }

    /// `nativeGetLocales`.
    pub fn get_locales(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for src in &self.sources {
            if let Some(table) = src.resource_table() {
                for c in table.all_configurations(src.resource_bytes().unwrap_or(&[])) {
                    if c.language[0] != 0 {
                        let lang: String = c
                            .language
                            .iter()
                            .take_while(|&&b| b != 0)
                            .map(|&b| b as char)
                            .collect();
                        if !out.contains(&lang) {
                            out.push(lang);
                        }
                    }
                }
            }
        }
        out.sort();
        out
    }

    // ---- asset handles -------------------------------------------------

    /// `nativeOpenNonAsset`.
    pub fn open_non_asset(&mut self, cookie: i32, filename: &str, _access_mode: i32) -> Result<u64> {
        let src = self
            .source_for_cookie(cookie)
            .ok_or_else(|| ResError::new(RES_BAD_HANDLE, format!("cookie {cookie}")))?;
        let data = src.read_entry(filename)?;
        self.next_asset += 1;
        let handle = self.next_asset;
        self.assets
            .insert(handle, AssetHandle::new(cookie, filename, data));
        Ok(handle)
    }

    /// `nativeOpenAsset` — search every source.
    pub fn open_asset(&mut self, filename: &str, access_mode: i32) -> Result<u64> {
        let names: Vec<(usize, i32)> = self
            .sources
            .iter()
            .enumerate()
            .map(|(i, _)| (i, i as i32 + 1))
            .collect();
        for (cookie_idx, cookie) in names {
            let src = &self.sources[cookie_idx];
            if let Ok(data) = src.read_entry(filename) {
                self.next_asset += 1;
                let handle = self.next_asset;
                self.assets
                    .insert(handle, AssetHandle::new(cookie, filename, data));
                let _ = access_mode;
                return Ok(handle);
            }
        }
        Err(ResError::new(
            crate::res::error::RES_ASSET_NOT_FOUND,
            filename,
        ))
    }

    /// The asset handle, if any.
    pub fn asset(&self, handle: u64) -> Option<&AssetHandle> {
        self.assets.get(&handle)
    }

    /// Mutable asset handle.
    pub fn asset_mut(&mut self, handle: u64) -> Option<&mut AssetHandle> {
        self.assets.get_mut(&handle)
    }

    /// `nativeAssetDestroy`.
    pub fn asset_destroy(&mut self, handle: u64) {
        self.assets.remove(&handle);
    }

    /// `nativeContainsAllocatedTable`.
    pub fn contains_allocated_table(&self) -> bool {
        !self.sources.is_empty()
    }

    // ---- XML -----------------------------------------------------------

    /// `nativeOpenXmlAsset` — read and parse a compiled XML entry.
    pub fn open_xml_asset(&mut self, cookie: i32, filename: &str) -> Result<u64> {
        let src = self
            .source_for_cookie(cookie)
            .ok_or_else(|| ResError::new(RES_BAD_HANDLE, format!("cookie {cookie}")))?;
        let bytes = src.read_entry(filename)?;
        let block = XmlBlock::from_bytes(&bytes, 0, bytes.len())?;
        self.next_block += 1;
        let handle = self.next_block;
        self.blocks.insert(handle, block);
        Ok(handle)
    }

    /// Insert a pre-parsed `XmlBlock` (the `XmlBlock.nativeCreate` path).
    pub fn create_xml_block(&mut self, block: XmlBlock) -> u64 {
        self.next_block += 1;
        let handle = self.next_block;
        self.blocks.insert(handle, block);
        handle
    }

    /// The XML block for a handle.
    pub fn xml_block(&self, handle: u64) -> Option<&XmlBlock> {
        self.blocks.get(&handle)
    }

    /// `XmlBlock.nativeDestroy`.
    pub fn destroy_xml_block(&mut self, handle: u64) {
        self.blocks.remove(&handle);
        self.parse_states.retain(|_, s| s.block != handle);
    }

    /// `XmlBlock.nativeCreateParseState`.
    pub fn create_parse_state(&mut self, block: u64, res_id: i32) -> Result<u64> {
        if !self.blocks.contains_key(&block) {
            return Err(ResError::new(RES_BAD_HANDLE, format!("xml block {block}")));
        }
        self.next_parse_state += 1;
        let handle = self.next_parse_state;
        self.parse_states
            .insert(handle, XmlParserState::new(block, res_id));
        Ok(handle)
    }

    /// `XmlBlock.nativeDestroyParseState`.
    pub fn destroy_parse_state(&mut self, handle: u64) {
        self.parse_states.remove(&handle);
    }

    /// The parse state for a handle.
    pub fn parse_state(&self, handle: u64) -> Option<&XmlParserState> {
        self.parse_states.get(&handle)
    }

    /// Mutable parse state.
    pub fn parse_state_mut(&mut self, handle: u64) -> Option<&mut XmlParserState> {
        self.parse_states.get_mut(&handle)
    }

    /// Run `nativeNext` for a parse state, using its owning block.
    pub fn parse_next(&mut self, handle: u64) -> Option<i32> {
        let block = self.parse_states.get(&handle)?.block;
        let doc = &self.blocks.get(&block)?.doc;
        // NOTE: the borrow checker needs the doc to outlive the state mutation;
        // clone the event code computation by taking the state out temporarily.
        let mut state = self.parse_states.remove(&handle)?;
        let code = state.native_next(doc);
        self.parse_states.insert(handle, state);
        Some(code)
    }

    // ---- themes --------------------------------------------------------

    /// Create a theme and return its handle.
    pub fn theme_create(&mut self) -> u64 {
        self.next_theme += 1;
        let handle = self.next_theme;
        let theme = Theme::new(self.config.clone());
        self.themes.insert(handle, theme);
        if self.default_theme == 0 {
            self.default_theme = handle;
        }
        handle
    }

    /// The default theme handle.
    pub fn default_theme(&self) -> u64 {
        self.default_theme
    }

    /// Look up a theme.
    pub fn theme(&self, handle: u64) -> Option<&Theme> {
        self.themes.get(&handle)
    }

    /// Mutable theme.
    pub fn theme_mut(&mut self, handle: u64) -> Option<&mut Theme> {
        self.themes.get_mut(&handle)
    }

    /// `nativeThemeCopy`.
    pub fn theme_copy(&mut self, dst: u64, src: u64) {
        if let Some(theme) = self.themes.get(&src).cloned() {
            self.themes.insert(dst, theme);
        }
    }

    /// `nativeThemeCopy` across two `AssetManager` instances: clone the source
    /// theme (which may live in a different manager) into `dst` on `self`.
    /// Used by `AssetManager.setThemeTo(dst, srcAsset, srcTheme)`.
    pub fn import_theme_from(&mut self, dst: u64, src: &AssetManager, src_theme: u64) -> bool {
        match src.themes.get(&src_theme).cloned() {
            Some(theme) => {
                self.themes.insert(dst, theme);
                true
            }
            None => false,
        }
    }

    /// Resolve an attribute through a theme (helper for reference chasing).
    pub fn theme_resolve_attr(&self, handle: u64, attr_id: u32) -> Option<ResValue> {
        let theme = self.themes.get(&handle)?;
        theme.get(attr_id)
    }

    /// `nativeApplyStyle` is implemented in `theme.rs` (it fills the output
    /// arrays without mutating the theme).
    /// `nativeThemeApplyStyle`.
    pub fn theme_apply_style(&mut self, theme: u64, res_id: u32, force: bool) {
        let Some(mut th) = self.themes.remove(&theme) else {
            return;
        };
        th.apply_style_id(self, res_id, force);
        self.themes.insert(theme, th);
    }

    /// `nativeThemeRebase`.
    pub fn theme_rebase(&mut self, theme: u64, style_ids: &[u32], force: &[bool]) {
        let Some(mut th) = self.themes.remove(&theme) else {
            return;
        };
        th.rebase(self, style_ids, force);
        self.themes.insert(theme, th);
    }

    /// `nativeThemeGetAttributeValue`.
    pub fn theme_get_attribute_value(
        &self,
        theme: u64,
        attr_id: u32,
        resolve: bool,
    ) -> Option<(i32, TypedValueOut)> {
        let value = self.themes.get(&theme)?.get(attr_id)?;
        let requested = self.themes.get(&theme).map(|t| &t.config).unwrap_or(&self.config);
        // With `resolve = true`, real ART resolves a `TYPE_REFERENCE` theme value
        // to the concrete resource (e.g. `colorBackground` -> ARGB8,
        // `textColorPrimary` -> the ColorStateList file path string). Only the
        // `Attr` chain (`?attr`) is followed while merging the style; `@resid`
        // references are resolved here, at query time.
        if value.is_reference() && resolve && value.data != 0 {
            if let Some((source_index, entry, final_id)) =
                self.resolve_references_with(value.data, requested, true)
            {
                return self
                    .build_typed_value(source_index, &entry, attr_id, final_id, requested.density)
                    .map(|(c, mut v)| {
                        v.density = requested.density;
                        (c, v)
                    });
            }
        }
        // Find the entry that provided the value so we can report a cookie.
        let (source_index, final_id) = if value.is_reference() && resolve {
            self.lookup_with(value.data, requested)
                .map(|h| (h.source_index, value.data))
                .unwrap_or((0, attr_id))
        } else {
            // Look up the attribute id itself to get its config/cookie.
            self.lookup_with(attr_id, requested)
                .map(|h| (h.source_index, attr_id))
                .unwrap_or((0, attr_id))
        };
        let mut entry = ResolvedEntry {
            value,
            bag: None,
            config: requested.clone(),
            config_label: requested.label(),
            changing_configurations: 0,
            package_name: String::new(),
            type_name: String::new(),
            key: String::new(),
            entry_id: 0,
            type_id: 0,
        };
        if let Some(hit) = self.lookup_with(attr_id, requested) {
            entry.changing_configurations = hit.entry.changing_configurations;
        }
        let _ = final_id;
        self.build_typed_value(source_index, &entry, attr_id, attr_id, 0)
            .map(|(c, mut v)| {
                v.density = requested.density;
                (c, v)
            })
    }
}
