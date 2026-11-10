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
use crate::res::value::{ResValue, TypedValueOut, STYLE_NUM_ENTRIES, TYPE_ATTRIBUTE};
use crate::res::xmlblock::{XmlEvent, XmlParserState};

/// A resolved theme: attribute id -> value, plus the ordered list of applied
/// styles (used for `getTheme()` and `rebase`).
#[derive(Debug, Clone)]
pub struct Theme {
    /// Configuration the theme was created with.
    pub config: ResConfig,
    /// Attribute id -> raw value.
    pub values: HashMap<u32, ResValue>,
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
        self.values.get(&attr_id).copied()
    }

    /// The value for `attr_id`, resolving `?attr` chains (but not `@resid`).
    pub fn get(&self, attr_id: u32) -> Option<ResValue> {
        self.get_inner(attr_id, 0)
    }

    fn get_inner(&self, attr_id: u32, depth: usize) -> Option<ResValue> {
        if depth > MAX_REFERENCE_DEPTH {
            return None;
        }
        let value = *self.values.get(&attr_id)?;
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

    fn merge(&mut self, map: HashMap<u32, ResValue>, force: bool) {
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
/// The parent is inserted first so the child overrides it.
pub(crate) fn collect_style(
    mgr: &AssetManager,
    config: &ResConfig,
    res_id: u32,
    depth: usize,
) -> HashMap<u32, ResValue> {
    let mut out = HashMap::new();
    if depth > MAX_REFERENCE_DEPTH {
        return out;
    }
    let Some(bag) = mgr.bag_for(res_id, config) else {
        return out;
    };
    if bag.parent != 0 && bag.parent != res_id {
        out.extend(collect_style(mgr, config, bag.parent, depth + 1));
    }
    for (name, value) in bag.entries {
        out.insert(name, value);
    }
    out
}

/// Extract the current start tag's attributes as `attr id -> value`.
fn xml_attr_map(state: &XmlParserState, doc: &crate::res::xmlblock::XmlDocument) -> HashMap<u32, ResValue> {
    let mut map = HashMap::new();
    if let Some(XmlEvent::StartTag(tag)) = doc.events.get(state.position) {
        for a in &tag.attributes {
            if a.resource_id != 0 {
                map.insert(a.resource_id, a.value);
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
        for i in 0..n {
            let attr = attrs[i] as u32;
            match map.get(&attr) {
                Some((value, idx)) => {
                    let tv = TypedValueOut {
                        data_type: value.data_type,
                        data: value.data,
                        asset_cookie: -1,
                        resource_id: attr,
                        changing_configurations: 0,
                        density: 0,
                        source_resource_id: attr,
                        flags: 0,
                        string: None,
                    };
                    tv.write_style(&mut out_values[i * STYLE_NUM_ENTRIES..]);
                    if i < out_indices.len() {
                        out_indices[i] = *idx;
                    }
                }
                None => {
                    if i < out_indices.len() {
                        out_indices[i] = -1;
                    }
                }
            }
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
        let mut sources: Vec<HashMap<u32, ResValue>> = Vec::new();

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

        self.fill_outputs(attrs, &sources, out_values, out_indices, true);
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
        let mut sources: Vec<HashMap<u32, ResValue>> = Vec::new();
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
        let n = attrs.len();
        for i in 0..n {
            let attr = attrs[i] as u32;
            let wanted = in_values
                .and_then(|v| v.get(i))
                .map(|v| *v as u32)
                .filter(|v| *v != 0)
                .unwrap_or(attr);
            let resolved = themed
                .get(wanted)
                .or_else(|| {
                    sources
                        .iter()
                        .find_map(|m| m.get(&wanted).copied())
                });
            if let Some(value) = resolved {
                let tv = TypedValueOut {
                    data_type: value.data_type,
                    data: value.data,
                    asset_cookie: -1,
                    resource_id: attr,
                    changing_configurations: 0,
                    density: config.density,
                    source_resource_id: attr,
                    flags: 0,
                    string: None,
                };
                tv.write_style(&mut out_values_direct[i * STYLE_NUM_ENTRIES..]);
                if i < out_indices.len() {
                    out_indices[i] = i as i32;
                }
            } else if i < out_indices.len() {
                out_indices[i] = -1;
            }
        }
        let copy = out_values.len().min(out_values_direct.len());
        out_values[..copy].copy_from_slice(&out_values_direct[..copy]);
        true
    }

    fn fill_outputs(
        &self,
        attrs: &[i32],
        sources: &[HashMap<u32, ResValue>],
        out_values: &mut [i32],
        out_indices: &mut [i32],
        zero_missing: bool,
    ) {
        let n = attrs.len().min(out_values.len() / STYLE_NUM_ENTRIES);
        for i in 0..n {
            let attr = attrs[i] as u32;
            let mut found = None;
            for (src_index, map) in sources.iter().enumerate() {
                if let Some(value) = map.get(&attr).copied() {
                    found = Some((src_index, value));
                    break;
                }
            }
            if let Some((src_index, value)) = found {
                let tv = TypedValueOut {
                    data_type: value.data_type,
                    data: value.data,
                    asset_cookie: -1,
                    resource_id: attr,
                    changing_configurations: 0,
                    density: 0,
                    source_resource_id: attr,
                    flags: 0,
                    string: None,
                };
                tv.write_style(&mut out_values[i * STYLE_NUM_ENTRIES..]);
                if i < out_indices.len() {
                    out_indices[i] = src_index as i32;
                }
            } else if i < out_indices.len() {
                out_indices[i] = -1;
                if zero_missing {
                    for slot in &mut out_values[i * STYLE_NUM_ENTRIES..(i + 1) * STYLE_NUM_ENTRIES] {
                        *slot = 0;
                    }
                }
            }
        }
    }
}
