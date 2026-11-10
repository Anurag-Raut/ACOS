#![allow(clippy::field_reassign_with_default)]
//! `XmlBlock` differential test.
//!
//! For `framework-res.apk:res/layout/screen_simple.xml` and the hello-views
//! layout we
//! (a) render the `XmlBlock` event stream to the exact text shape produced by
//!     `acos-apk`'s AXML parser (`acos_apk::axml::to_xmltree`, which is itself
//!     differential-tested against `aapt2`) and compare byte-for-byte, and
//! (b) compare the element/attribute *structure* (names, namespaces, resource
//!     ids, line numbers) against `aapt2 dump xmltree`.
//!
//! It also counts the `nativeNext` calls needed to walk each document.

use std::path::PathBuf;
use std::process::Command;

use acos_framework_natives::res::{
    ApkAssets, AssetSource, ResValue, StringBlock, XmlBlock, XmlEvent, XmlParserState, END_DOCUMENT,
};

fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

fn find_aapt2() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AAPT2") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)?
        .join("Android/Sdk/build-tools");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&base)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    versions.sort();
    for v in versions.into_iter().rev() {
        let candidate = v.join("aapt2");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

const DIM_UNITS: [&str; 6] = ["px", "dip", "sp", "pt", "in", "mm"];
const FRAC_UNITS: [&str; 2] = ["%", "%p"];

fn complex_to_float(complex: u32, units: &[&str]) -> (f32, String) {
    const MANTISSA_MULT: f32 = 1.0 / (1 << 8) as f32;
    const RADIX_MULTS: [f32; 4] = [
        1.0 * MANTISSA_MULT,
        1.0 / (1 << 7) as f32 * MANTISSA_MULT,
        1.0 / (1 << 15) as f32 * MANTISSA_MULT,
        1.0 / (1 << 23) as f32 * MANTISSA_MULT,
    ];
    let radix = ((complex >> 4) & 0x3) as usize;
    let value = (complex & (0xFF_FFFF << 8)) as f32 * RADIX_MULTS[radix];
    let unit = units.get((complex & 0xF) as usize).copied().unwrap_or("?").to_string();
    (value, unit)
}

/// Render a value exactly like `acos-apk`'s `TypedValue::display`.
fn display(strings: &StringBlock, value: ResValue) -> String {
    match value.data_type {
        0x00 => "null".to_string(),
        0x01 | 0x07 => format!("@{:#010x}", value.data),
        0x02 => format!("?{:#010x}", value.data),
        0x03 => strings.get(value.data as i32).unwrap_or("").to_string(),
        0x04 => f32::from_bits(value.data).to_string(),
        0x05 => {
            let (v, u) = complex_to_float(value.data, &DIM_UNITS);
            format!("{v}{u}")
        }
        0x06 => {
            let (v, u) = complex_to_float(value.data, &FRAC_UNITS);
            format!("{v}{u}")
        }
        0x10 => (value.data as i32).to_string(),
        0x11 => format!("0x{:08x}", value.data),
        0x12 => (value.data != 0).to_string(),
        0x1c..=0x1f => format!("#{:08x}", value.data),
        other => format!("<type {other:#04x}>:{:#010x}", value.data),
    }
}

/// Walk the event stream and emit `acos-apk`'s `to_xmltree` shape.
fn my_xmltree(block: &XmlBlock) -> String {
    let doc = &block.doc;
    let strings = &doc.string_block;
    let mut out = String::new();
    let mut depth = 0usize;
    for event in &doc.events {
        match event {
            XmlEvent::StartTag(tag) => {
                let indent = "  ".repeat(depth);
                let name = strings.get(tag.name_index).unwrap_or("");
                out.push_str(&format!("{indent}E: {name} (line={})\n", tag.line));
                for a in &tag.attributes {
                    let ns = if a.namespace_index >= 0 {
                        strings.get(a.namespace_index).map(str::to_string)
                    } else {
                        None
                    };
                    let attr_name = strings.get(a.name_index).unwrap_or("");
                    let qual = match (&ns, a.resource_id) {
                        (Some(ns), id) if id != 0 => {
                            format!("{ns}:{attr_name}(0x{id:08x})")
                        }
                        (Some(ns), _) => format!("{ns}:{attr_name}"),
                        (None, _) => attr_name.to_string(),
                    };
                    out.push_str(&format!(
                        "{indent}  A: {qual}={}\n",
                        display(strings, a.value)
                    ));
                }
                depth += 1;
            }
            XmlEvent::EndTag { .. } => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    out
}

fn count_native_next(block: &XmlBlock) -> usize {
    let mut state = XmlParserState::new(0, 0);
    let _ = state.next_event(&block.doc); // Java next() returns START_DOCUMENT
    let mut count = 0;
    loop {
        let ev = state.native_next(&block.doc);
        count += 1;
        if ev == END_DOCUMENT {
            break;
        }
        if count > 100_000 {
            break;
        }
    }
    count
}

#[derive(Debug, PartialEq, Eq)]
struct Node {
    depth: usize,
    name: String,
    line: u32,
    attrs: Vec<(String, String, u32)>,
}

fn normalize_aapt2(output: &str) -> Vec<Node> {
    let mut nodes: Vec<Node> = Vec::new();
    for line in output.lines() {
        if line.starts_with("N:") {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        // aapt2 indents elements at 2 + 4*depth spaces (attributes +2).
        let depth = indent.saturating_sub(2) / 4;
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("E: ") {
            let name = rest.split(" (line=").next().unwrap_or(rest).to_string();
            let line_no = rest
                .split("(line=")
                .nth(1)
                .and_then(|s| s.trim_end_matches(')').parse::<u32>().ok())
                .unwrap_or(0);
            nodes.push(Node {
                depth,
                name,
                line: line_no,
                attrs: Vec::new(),
            });
        } else if let Some(rest) = trimmed.strip_prefix("A: ") {
            let (key, _value) = rest.split_once('=').unwrap_or((rest, ""));
            let (ns, name, resid) = parse_aapt2_attr(key);
            if let Some(last) = nodes.last_mut() {
                last.attrs.push((ns, name, resid));
            }
        }
    }
    nodes
}

/// Parse `http://...:name(0x010100c4)` or `name` into `(ns, local, resid)`.
fn parse_aapt2_attr(key: &str) -> (String, String, u32) {
    let (name_part, resid) = match key.rfind('(') {
        Some(i) if key.ends_with(')') => {
            let id_hex = key[i + 1..key.len() - 1].trim_start_matches("0x");
            (&key[..i], u32::from_str_radix(id_hex, 16).unwrap_or(0))
        }
        _ => (key, 0),
    };
    match name_part.rfind(':') {
        Some(i) if name_part[..i].contains('/') => (
            name_part[..i].to_string(),
            name_part[i + 1..].to_string(),
            resid,
        ),
        _ => (String::new(), name_part.to_string(), resid),
    }
}

fn my_nodes(block: &XmlBlock) -> Vec<Node> {
    let doc = &block.doc;
    let strings = &doc.string_block;
    let mut nodes = Vec::new();
    let mut depth = 0usize;
    for event in &doc.events {
        match event {
            XmlEvent::StartTag(tag) => {
                let name = strings.get(tag.name_index).unwrap_or("").to_string();
                let mut attrs = Vec::new();
                for a in &tag.attributes {
                    let ns = if a.namespace_index >= 0 {
                        strings.get(a.namespace_index).unwrap_or("").to_string()
                    } else {
                        String::new()
                    };
                    attrs.push((
                        ns,
                        strings.get(a.name_index).unwrap_or("").to_string(),
                        a.resource_id,
                    ));
                }
                nodes.push(Node {
                    depth,
                    name,
                    line: tag.line,
                    attrs,
                });
                depth += 1;
            }
            XmlEvent::EndTag { .. } => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    nodes
}

fn check_layout(apk_path: &PathBuf, entry: &str) {
    let apk = ApkAssets::open(apk_path).unwrap();
    let bytes = apk.read_entry(entry).expect("read xml entry");
    let block = XmlBlock::from_bytes(&bytes, 0, bytes.len()).expect("parse AXML");

    // (a) full text vs acos-apk's own parser.
    let mine = my_xmltree(&block);
    let doc = acos_apk::axml::parse(&bytes).expect("acos-apk parse");
    let theirs = acos_apk::axml::to_xmltree(&doc);
    assert_eq!(mine, theirs, "{entry}: XmlBlock stream != acos-apk stream");

    // (c) nativeNext count.
    let count = count_native_next(&block);
    eprintln!(
        "XmlBlock {entry}: {} events, {count} nativeNext calls",
        block.doc.events.len()
    );

    // (b) structural comparison against aapt2.
    if let Some(aapt2) = find_aapt2() {
        let output = Command::new(&aapt2)
            .args(["dump", "xmltree"])
            .arg(apk_path)
            .args(["--file", entry])
            .output()
            .expect("run aapt2");
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let aapt_nodes = normalize_aapt2(&text);
            let mine_nodes = my_nodes(&block);
            assert_eq!(
                mine_nodes, aapt_nodes,
                "{entry}: XmlBlock structure != aapt2 xmltree"
            );
        } else {
            eprintln!("aapt2 xmltree failed for {entry}; skipping aapt2 check");
        }
    }
}

#[test]
fn xmlblock_screen_simple() {
    let apk = workspace_root().join(".cache/boot/android-34/framework-res.apk");
    if !apk.exists() {
        eprintln!("framework-res.apk missing; skipping");
        return;
    }
    check_layout(&apk, "res/layout/screen_simple.xml");
}

#[test]
fn xmlblock_hello_views_layout() {
    let apk = workspace_root().join("fixtures/build/apps/hello-views.apk");
    if !apk.exists() {
        eprintln!("hello-views.apk missing; skipping");
        return;
    }
    check_layout(&apk, "res/layout/main.xml");
}

#[test]
fn retrieve_attributes_through_manager() {
    let apk_path = workspace_root().join("fixtures/build/apps/hello-views.apk");
    if !apk_path.exists() {
        eprintln!("hello-views.apk missing; skipping");
        return;
    }
    let mut mgr = acos_framework_natives::res::AssetManager::new();
    mgr.add_apk_assets(ApkAssets::open(&apk_path).unwrap());
    let bytes = mgr.sources()[0]
        .read_entry("res/layout/main.xml")
        .expect("layout");
    let block = mgr.create_xml_block(XmlBlock::from_bytes(&bytes, 0, bytes.len()).unwrap());
    let state = mgr.create_parse_state(block, 0x7f020000).unwrap();
    // Advance to the first START_TAG (LinearLayout).
    loop {
        let ev = mgr.parse_next(state).unwrap();
        if ev == acos_framework_natives::res::START_TAG {
            break;
        }
        assert_ne!(ev, acos_framework_natives::res::END_DOCUMENT);
    }
    assert_eq!(mgr.parse_state(state).unwrap().get_source_res_id(), 0x7f020000);
    let attrs = [0x0101_00c4i32, 0x0101_00d5, 0x0101_00f4, 0x0101_00f5];
    let mut out_values = vec![0i32; attrs.len() * 7];
    let mut out_indices = vec![-1i32; attrs.len()];
    assert!(mgr.retrieve_attributes(state, &attrs, &mut out_values, &mut out_indices));
    // orientation = int 1
    assert_eq!(out_values[0], 0x10);
    assert_eq!(out_values[1], 1);
    // padding = dimension 16dp (16 << 8 | 1)
    assert_eq!(out_values[7], 0x05);
    assert_eq!(out_values[8], 0x1001);
    // layout_width = -1 (MATCH_PARENT)
    assert_eq!(out_values[14], 0x10);
    assert_eq!(out_values[15], -1);
    // Out-of-range/missing attributes get -1; here all four exist and are
    // reported at their index within the XML start tag.
    assert_eq!(out_indices, vec![0, 1, 2, 3]);
    eprintln!("retrieveAttributes: orientation=1 padding=16dp layout_width=-1");
}
