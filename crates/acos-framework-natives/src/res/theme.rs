//! Theme and style resolution.
//!
//! Ports the observable behaviour of `androidfw` `ResTable::Theme`:
//! `nativeApplyStyle` (XML tag + `defStyleAttr` + `defStyleRes` + theme),
//! `nativeThemeApplyStyle`/`nativeThemeCopy`/`nativeThemeRebase`,
//! `nativeResolveAttrs`, `nativeRetrieveAttributes`, and the 7-int
//! `TypedArray` layout.

use std::collections::HashMap;

use crate::res::config::ResConfig;
use crate::res::manager::{AssetManager, MAX_REFERENCE_DEPTH};
use crate::res::value::{
    ResValue, TypedValueOut, STYLE_NUM_ENTRIES, TYPE_ATTRIBUTE, TYPE_DYNAMIC_ATTRIBUTE,
    TYPE_DYNAMIC_REFERENCE, TYPE_NULL, TYPE_REFERENCE, TYPE_STRING,
};
use crate::res::xmlblock::{XmlEvent, XmlParserState};

/// Provenance of a resolved resource value.
///
/// `None` means the value came from the layout XML start tag, so a
/// `TYPE_STRING` `data` is an index into the parser's own `StringBlock` and the
/// value keeps asset cookie `-1`. `Some(source_index)` means it came from a
/// style/theme bag in `AssetManager::sources[source_index]`, so `data` indexes
/// that APK's ARSC global string pool and the value must be written with
/// `assetCookie == source_index + 1`.
pub(crate) type TaggedValue = (Option<usize>, ResValue);
/// An `attr id -> (provenance, value)` map.
pub(crate) type TaggedMap = HashMap<u32, TaggedValue>;

/// A resolved theme: attribute id -> value, plus the ordered list of applied
/// styles (used for `getTheme()` and `rebase`).
#[derive(Debug, Clone)]
pub struct Theme {
    /// Configuration the theme was created with.
    pub config: ResConfig,
    /// Attribute id -> (provenance, raw value).
    pub values: TaggedMap,
    /// Applied style resource ids, in application order.
    pub styles: Vec<u32>,
    /// Whether each applied style was forced.
    pub force: Vec<bool>,
}

impl Theme {
    /// Create an empty theme.
    pub fn new(config: ResConfig) -> Self {
        Theme {
            config,
            values: HashMap::new(),
            styles: Vec::new(),
            force: Vec::new(),
        }
    }

    /// The raw value for `attr_id`, if the theme defines it.
    pub fn get_raw(&self, attr_id: u32) -> Option<ResValue> {
        self.values.get(&attr_id).map(|(_, v)| *v)
    }

    /// The value for `attr_id`, resolving `?attr` chains (but not `@resid`).
    pub fn get(&self, attr_id: u32) -> Option<ResValue> {
        self.get_inner(attr_id, 0)
    }

    /// Like [`Theme::get`], but keeps the provenance of the final value.
    pub(crate) fn get_tagged(&self, attr_id: u32) -> Option<TaggedValue> {
        self.get_tagged_inner(attr_id, 0)
    }

    fn get_tagged_inner(&self, attr_id: u32, depth: usize) -> Option<TaggedValue> {
        if depth > MAX_REFERENCE_DEPTH {
            return None;
        }
        let tagged = *self.values.get(&attr_id)?;
        match tagged.1.data_type {
            TYPE_ATTRIBUTE => self.get_tagged_inner(tagged.1.data, depth + 1),
            _ => Some(tagged),
        }
    }

    fn get_inner(&self, attr_id: u32, depth: usize) -> Option<ResValue> {
        if depth > MAX_REFERENCE_DEPTH {
            return None;
        }
        let (_, value) = *self.values.get(&attr_id)?;
        match value.data_type {
            TYPE_ATTRIBUTE => self.get_inner(value.data, depth + 1),
            _ => Some(value),
        }
    }

    /// Number of attributes defined.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// True when the theme has no attributes.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// `nativeThemeApplyStyle`: merge a style bag (with parent chain) into the
    /// theme. `force` controls whether an existing value is overwritten.
    pub fn apply_style_id(&mut self, mgr: &AssetManager, res_id: u32, force: bool) {
        self.styles.push(res_id);
        self.force.push(force);
        let map = collect_style(mgr, &self.config, res_id, 0);
        self.merge(map, force);
    }

    /// `nativeApplyStyle` applied to the theme itself (not the array output).
    pub fn apply_style(&mut self, mgr: &AssetManager, def_style_attr: u32, def_style_res: u32) {
        if def_style_attr != 0 {
            if let Some(v) = self.get(def_style_attr) {
                if v.is_reference() {
                    let map = collect_style(mgr, &self.config, v.data, 0);
                    self.merge(map, false);
                }
            }
        }
        if def_style_res != 0 {
            let map = collect_style(mgr, &self.config, def_style_res, 0);
            self.merge(map, false);
        }
    }

    /// `nativeThemeRebase`: reapply the recorded styles from scratch.
    pub fn rebase(&mut self, mgr: &AssetManager, style_ids: &[u32], force: &[bool]) {
        let old = std::mem::take(&mut self.values);
        self.styles.clear();
        self.force.clear();
        for (i, id) in style_ids.iter().enumerate() {
            let f = force.get(i).copied().unwrap_or(false);
            self.apply_style_id(mgr, *id, f);
        }
        // Any value not re-applied falls back to the previous theme.
        for (k, v) in old {
            self.values.entry(k).or_insert(v);
        }
    }

    fn merge(&mut self, map: TaggedMap, force: bool) {
        for (k, v) in map {
            match self.values.get(&k) {
                Some(_) if !force => {}
                _ => {
                    self.values.insert(k, v);
                }
            }
        }
    }
}

/// Recursively merge a style bag and its parents into `attr id -> value`.
///
/// The parent is inserted first so the child overrides it. Every entry is
/// tagged with the [`AssetManager::sources`] index of the bag it came from, so
/// string values can be written with the matching asset cookie.
pub(crate) fn collect_style(
    mgr: &AssetManager,
    config: &ResConfig,
    res_id: u32,
    depth: usize,
) -> TaggedMap {
    let mut out = TaggedMap::new();
    if depth > MAX_REFERENCE_DEPTH {
        return out;
    }
    let Some(hit) = mgr.lookup_with(res_id, config) else {
        return out;
    };
    let source_index = hit.source_index;
    let Some(bag) = hit.entry.bag else {
        return out;
    };
    if bag.parent != 0 && bag.parent != res_id {
        out.extend(collect_style(mgr, config, bag.parent, depth + 1));
    }
    for (name, value) in bag.entries {
        out.insert(name, (Some(source_index), value));
    }
    out
}

/// Extract the current start tag's attributes as `attr id -> value`.
fn xml_attr_map(state: &XmlParserState, doc: &crate::res::xmlblock::XmlDocument) -> TaggedMap {
    let mut map = TaggedMap::new();
    if let Some(XmlEvent::StartTag(tag)) = doc.events.get(state.position) {
        for a in &tag.attributes {
            if a.resource_id != 0 {
                map.insert(a.resource_id, (None, a.value));
            }
        }
    }
    map
}

impl AssetManager {
    /// `nativeRetrieveAttributes`: read requested attributes from the current
    /// XML start tag (no theme involvement).
    pub fn retrieve_attributes(
        &self,
        parser_state: u64,
        attrs: &[i32],
        out_values: &mut [i32],
        out_indices: &mut [i32],
    ) -> bool {
        let Some(state) = self.parse_state(parser_state) else {
            return false;
        };
        let Some(block) = self.xml_block(state.block) else {
            return false;
        };
        // Requested attr id -> (value, index within the current start tag).
        let mut map: HashMap<u32, (ResValue, i32)> = HashMap::new();
        if let Some(XmlEvent::StartTag(tag)) = block.doc.events.get(state.position) {
            for (idx, a) in tag.attributes.iter().enumerate() {
                if a.resource_id != 0 {
                    map.insert(a.resource_id, (a.value, idx as i32));
                }
            }
        }
        let n = attrs.len().min(out_values.len() / STYLE_NUM_ENTRIES);
        // `TypedArray`s are pooled and `mData` is reused across tags. Clear
        // every slot before filling: `TypedArray.getValueAt` treats any
        // non-zero `STYLE_TYPE` as "present", so a slot an earlier tag wrote
        // (e.g. `android:width`) would otherwise be read back as this tag's
        // attribute. `apply_style`/`fill_outputs(zero_missing = true)` already
        // does this.
        for slot in &mut out_values[..n * STYLE_NUM_ENTRIES] {
            *slot = 0;
        }
        // `TypedArray` layout: `mIndices[0]` is the count of found attributes
        // and `mIndices[1..=count]` are the **request/styleable indices** of
        // those attributes (what `getIndex(at)` returns). `mData` stays indexed
        // by request index (`getValueAt(request_index)`).
        let mut count = 0i32;
        for i in 0..n {
            let attr = attrs[i] as u32;
            if let Some((value, _idx)) = map.get(&attr) {
                    // `nativeRetrieveAttributes` must chase a raw `@resid`
                    // attribute value to its concrete resource, exactly like
                    // `applyStyle` does. `Resources.obtainAttributes` with a
                    // null theme (what `ColorStateList.inflate` uses) routes
                    // here, and `TypedArray.getColor` raises `Can't convert
                    // value at index N to color: type=0x1` on a raw
                    // `TYPE_REFERENCE` (framework `res/color/*.xml`).
                    // `build_typed_value` also decodes a resolved string into
                    // the right asset cookie, so `TypedArray.getValueAt` reads
                    // the ARSC pool instead of the XML pool.
                    let built = match value.data_type {
                        TYPE_REFERENCE | TYPE_DYNAMIC_REFERENCE if value.data != 0 => self
                            .resolve_references(value.data, true)
                            .and_then(|(si, entry, fid)| {
                                self.build_typed_value(si, &entry, attr, fid, 0)
                            }),
                        _ => None,
                    };
                    let tv = match built {
                        Some((_cookie, tv)) => tv,
                        None => null_or_raw_typed_value(*value, attr),
                    };
                    tv.write_style(&mut out_values[i * STYLE_NUM_ENTRIES..]);
                    let slot = count as usize + 1;
                    if slot < out_indices.len() {
                        out_indices[slot] = i as i32;
                    }
                    count += 1;
            }
        }
        if !out_indices.is_empty() {
            out_indices[0] = count;
        }
        true
    }

    /// `nativeApplyStyle`: XML tag attributes, then `defStyleAttr`, then
    /// `defStyleRes`, then the theme's own values.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_style(
        &self,
        theme: u64,
        def_style_attr: u32,
        def_style_res: u32,
        parser_state: u64,
        attrs: &[i32],
        out_values: &mut [i32],
        out_indices: &mut [i32],
    ) {
        let config = self
            .theme(theme)
            .map(|t| t.config.clone())
            .unwrap_or_else(|| self.config().clone());
        let mut sources: Vec<TaggedMap> = Vec::new();

        // 1. XML attributes (highest priority).
        if parser_state != 0 {
            if let Some(state) = self.parse_state(parser_state) {
                if let Some(block) = self.xml_block(state.block) {
                    sources.push(xml_attr_map(state, &block.doc));
                }
            }
        }

        // 2. defStyleAttr resolved through the theme.
        if def_style_attr != 0 {
            let style_id = self
                .theme(theme)
                .and_then(|t| t.get(def_style_attr))
                .filter(|v| v.is_reference())
                .map(|v| v.data);
            if let Some(id) = style_id {
                sources.push(collect_style(self, &config, id, 0));
            }
        }

        // 3. defStyleRes.
        if def_style_res != 0 {
            sources.push(collect_style(self, &config, def_style_res, 0));
        }

        // 4. The theme itself.
        if let Some(t) = self.theme(theme) {
            sources.push(t.values.clone());
        }

        self.fill_outputs(theme, attrs, &sources, out_values, out_indices, true);
    }

    /// `nativeResolveAttrs`: resolve each `values[i]` (`?attr`) through the
    /// theme, with `defStyleAttr`/`defStyleRes` fallbacks.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_attrs(
        &self,
        theme: u64,
        def_style_attr: u32,
        def_style_res: u32,
        in_values: Option<&[i32]>,
        attrs: &[i32],
        out_values: &mut [i32],
        out_indices: &mut [i32],
    ) -> bool {
        let config = self
            .theme(theme)
            .map(|t| t.config.clone())
            .unwrap_or_else(|| self.config().clone());
        let mut sources: Vec<TaggedMap> = Vec::new();
        if def_style_attr != 0 {
            let style_id = self
                .theme(theme)
                .and_then(|t| t.get(def_style_attr))
                .filter(|v| v.is_reference())
                .map(|v| v.data);
            if let Some(id) = style_id {
                sources.push(collect_style(self, &config, id, 0));
            }
        }
        if def_style_res != 0 {
            sources.push(collect_style(self, &config, def_style_res, 0));
        }
        if let Some(t) = self.theme(theme) {
            sources.push(t.values.clone());
        }
        let Some(themed) = self.theme(theme) else {
            return false;
        };
        // Build the per-attr requested list from `in_values` when present.
        let mut out_values_direct = vec![0i32; attrs.len() * STYLE_NUM_ENTRIES];
        let mut out_count = 0i32;
        let n = attrs.len();
        for i in 0..n {
            let attr = attrs[i] as u32;
            let wanted = in_values
                .and_then(|v| v.get(i))
                .map(|v| *v as u32)
                .filter(|v| *v != 0)
                .unwrap_or(attr);
            let resolved = themed
                .get_tagged(wanted)
                .or_else(|| {
                    sources
                        .iter()
                        .find_map(|m| m.get(&wanted).copied())
                });
            if let Some((prov, value)) = resolved {
                // Chase the value to its concrete form, exactly like
                // `AssetManager2::ResolveAttributeReference` + `ResolveReference`:
                // a `?attr` chain is followed through the theme, then a `@resid`
                // through the resource table. `nativeResolveAttrs` previously
                // copied a resolved `TYPE_REFERENCE` verbatim, so
                // `Resources.Theme.resolveAttributes` (used by
                // `VectorDrawable.applyTheme`) handed a `TYPE_REFERENCE` to
                // `Resources.loadColorStateList`, whose `loadComplexColor` raised
                // `UnsupportedOperationException: Can't convert to ComplexColor:
                // type=0x1`.
                let (chained_prov, chained) = resolve_theme_attributes(themed, (prov, value));
                let built = if chained.is_reference() && chained.data != 0 {
                    self.resolve_references(chained.data, true).and_then(
                        |(source_index, entry, final_id)| {
                            self.build_typed_value(source_index, &entry, attr, final_id, 0)
                        },
                    )
                } else {
                    None
                };
                let tv = match built {
                    Some((_cookie, tv)) => tv,
                    None => self.raw_typed_value(chained, attr, chained_prov),
                };
                tv.write_style(&mut out_values_direct[i * STYLE_NUM_ENTRIES..]);
                let slot = out_count as usize + 1;
                if slot < out_indices.len() {
                    out_indices[slot] = i as i32;
                }
                out_count += 1;
            }
        }
        if !out_indices.is_empty() {
            out_indices[0] = out_count;
        }
        let copy = out_values.len().min(out_values_direct.len());
        out_values[..copy].copy_from_slice(&out_values_direct[..copy]);
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn fill_outputs(
        &self,
        theme: u64,
        attrs: &[i32],
        sources: &[TaggedMap],
        out_values: &mut [i32],
        out_indices: &mut [i32],
        zero_missing: bool,
    ) {
        let n = attrs.len().min(out_values.len() / STYLE_NUM_ENTRIES);
        let mut count = 0i32;
        for i in 0..n {
            let attr = attrs[i] as u32;
            let mut found = None;
            for map in sources {
                if let Some(&(prov, value)) = map.get(&attr) {
                    found = Some((prov, value));
                    break;
                }
            }
            if let Some((prov, value)) = found {
                // A `?attr` (from the XML or a style) is resolved through this
                // `TypedArray`'s theme first (`ResolveAttributeReference`),
                // then any resulting `@resid` through the resource table
                // (`ResolveReference`). A `@resid` is chased directly. The
                // theme value may be concrete (a dimension/color int), in
                // which case it is written through as-is. `build_typed_value`
                // supplies the right asset cookie and pooled string; concrete
                // XML values keep the `-1` cookie that routes string reads
                // through the parser's own `StringBlock`, while style/theme
                // strings keep the ARSC cookie for their source.
                let themed = if value.data_type == TYPE_ATTRIBUTE
                    || value.data_type == crate::res::value::TYPE_DYNAMIC_ATTRIBUTE
                {
                    self.theme(theme).map(|t| resolve_theme_attributes(t, (prov, value)))
                } else {
                    Some((prov, value))
                };
                let (prov, value) = themed.unwrap_or((prov, value));
                let built = match value {
                    v if v.data_type == TYPE_REFERENCE
                        || v.data_type == crate::res::value::TYPE_DYNAMIC_REFERENCE =>
                    {
                        if v.data == 0 {
                            None
                        } else {
                            self.resolve_references(v.data, true)
                        }
                    }
                    _ => None,
                }
                .and_then(|(si, entry, fid)| self.build_typed_value(si, &entry, attr, fid, 0));
                let tv = match built {
                    Some((_cookie, tv)) => tv,
                    None => self.raw_typed_value(value, attr, prov),
                };
                tv.write_style(&mut out_values[i * STYLE_NUM_ENTRIES..]);
                let slot = count as usize + 1;
                if slot < out_indices.len() {
                    out_indices[slot] = i as i32;
                }
                count += 1;
            } else if zero_missing {
                for slot in &mut out_values[i * STYLE_NUM_ENTRIES..(i + 1) * STYLE_NUM_ENTRIES] {
                    *slot = 0;
                }
            }
        }
        if !out_indices.is_empty() {
            out_indices[0] = count;
        }
    }

    /// Build the `TypedValue` for a concrete (non-`@resid`) value, keeping the
    /// asset cookie required to read a style/theme string from the ARSC pool.
    fn raw_typed_value(&self, value: ResValue, attr: u32, prov: Option<usize>) -> TypedValueOut {
        match prov {
            Some(source_index) => self.res_table_typed_value(value, attr, source_index),
            None => null_or_raw_typed_value(value, attr),
        }
    }

    fn res_table_typed_value(
        &self,
        value: ResValue,
        attr: u32,
        source_index: usize,
    ) -> TypedValueOut {
        if value.is_reference() && value.data == 0 {
            return null_or_raw_typed_value(value, attr);
        }
        let mut out = null_or_raw_typed_value(value, attr);
        out.asset_cookie = source_index as i32 + 1;
        if value.data_type == TYPE_STRING {
            out.string = self.pooled_string(source_index, value.data);
        }
        out
    }
}

/// Follow a `?attr` chain through the theme's own values, stopping at the
/// first non-attribute (or on a depth/cycle bound). `@resid` references are
/// left for [`AssetManager::resolve_references`], which owns the resource-table
/// lookup. The provenance of the final value is preserved.
fn resolve_theme_attributes(theme: &Theme, mut tagged: TaggedValue) -> TaggedValue {
    for _ in 0..MAX_REFERENCE_DEPTH {
        match tagged.1.data_type {
            TYPE_ATTRIBUTE | TYPE_DYNAMIC_ATTRIBUTE => match theme.values.get(&tagged.1.data) {
                Some(&next) => tagged = next,
                None => break,
            },
            _ => break,
        }
    }
    tagged
}

/// The `TypedValue` for a raw style value that did not resolve.
///
/// `@null` / `?null` is a reference whose target id is `0`; the framework's
/// `TypedArray.getDrawable`/`getColor` branch on `TYPE_REFERENCE` and would
/// call `Resources.loadDrawable(value, 0, theme)`, which throws
/// `Resources$NotFoundException: Unable to find resource ID #0x0`. A null
/// reference must be materialised as `TYPE_NULL` instead (e.g.
/// `windowContentOverlay=@null` in `ActionBarOverlayLayout.init`).
fn null_or_raw_typed_value(value: ResValue, attr: u32) -> TypedValueOut {
    if value.is_reference() && value.data == 0 {
        return TypedValueOut {
            data_type: TYPE_NULL,
            data: 0,
            asset_cookie: -1,
            resource_id: 0,
            changing_configurations: 0,
            density: 0,
            source_resource_id: attr,
            flags: 0,
            string: None,
        };
    }
    TypedValueOut {
        data_type: value.data_type,
        data: value.data,
        asset_cookie: -1,
        // A `@resid` carries its target in `STYLE_RESOURCE_ID` so
        // `TypedArray.getResourceId` returns the referenced resource, not the
        // attribute id. A raw value has no resource id, and Android 14's
        // `TypedArray.getResourceId` reads this slot verbatim, so storing the
        // attribute id here made `View` treat e.g. `android:pointerIcon=1002`
        // as `@0x01010509` and inflate `attr/pointerIcon` as a drawable
        // (`Resources$NotFoundException: Resource ID #0x1010509 type #0x1 is
        // not valid` while inflating `date_picker_header_material`).
        resource_id: if value.is_reference() { value.data } else { 0 },
        changing_configurations: 0,
        density: 0,
        source_resource_id: attr,
        flags: 0,
        string: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::res::value::TYPE_INT_DEC;

    #[test]
    fn raw_style_value_carries_no_resource_id() {
        // `android:pointerIcon="1002"` is a raw system-icon type, not a
        // `@resid`. Android 14's `TypedArray.getResourceId` reads
        // `STYLE_RESOURCE_ID`; it must stay 0 so the framework does not try to
        // load the attribute itself as a drawable.
        let raw = ResValue {
            data_type: TYPE_INT_DEC,
            data: 1002,
        };
        let tv = null_or_raw_typed_value(raw, 0x0101_0509);
        assert_eq!(tv.data_type, TYPE_INT_DEC);
        assert_eq!(tv.data, 1002);
        assert_eq!(tv.resource_id, 0);
        assert_eq!(tv.source_resource_id, 0x0101_0509);
    }

    #[test]
    fn reference_style_value_targets_its_resource() {
        let reference = ResValue::reference(0x7f08_0001);
        let tv = null_or_raw_typed_value(reference, 0x0101_0509);
        assert_eq!(tv.resource_id, 0x7f08_0001);
    }
}
