//! Deterministic generation of the AIDL static table and the full-boot JSON
//! dump.
//!
//! Everything here is driven by real files: the boot classpath
//! (`BOOTCLASSPATH` plus `dex/<jar>/classes*.dex`) and the measured work-lists
//! (`services.tsv`, `binder_calls_*.tsv`). Output ordering is fully
//! deterministic (interfaces sorted by descriptor, methods by transaction
//! code), so regeneration is byte-identical.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use acos_dex::DexFile;

use crate::decode::{descriptor_of, DecodeError, Scanner};
use crate::ir::{InterfaceInfo, MethodInfo, WireOp};

/// Errors from the generator.
#[derive(Debug, thiserror::Error)]
pub enum GenError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("dex {path}: {err}")]
    Dex {
        path: String,
        err: acos_dex::DexError,
    },
    #[error("missing boot classpath file: {0}")]
    Boot(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// A DEX file loaded from the boot classpath, with its originating jar.
#[derive(Debug)]
pub struct LoadedDex {
    pub jar: String,
    pub path: PathBuf,
    pub dex: DexFile,
}

/// The boot classpath in `BOOTCLASSPATH` order.
#[derive(Debug)]
pub struct BootScanner {
    pub root: PathBuf,
    pub jars: Vec<String>,
    pub dexes: Vec<LoadedDex>,
}

/// Numeric suffix of `classes.dex` (1), `classes2.dex` (2), ...
fn dex_number(name: &str) -> u32 {
    let stem = name.trim_start_matches("classes").trim_end_matches(".dex");
    if stem.is_empty() {
        1
    } else {
        stem.parse().unwrap_or(u32::MAX)
    }
}

impl BootScanner {
    /// Load and parse every boot-classpath DEX under `root` (e.g.
    /// `.cache/boot/android-34`).
    pub fn load(root: impl AsRef<Path>) -> Result<BootScanner, GenError> {
        let root = root.as_ref().to_path_buf();
        let bootcp_path = root.join("BOOTCLASSPATH");
        let bootcp = std::fs::read_to_string(&bootcp_path)
            .map_err(|_| GenError::Boot(bootcp_path.display().to_string()))?;
        let jars: Vec<String> = bootcp
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();

        let mut dexes = Vec::new();
        for jar in &jars {
            let stem = jar.trim_end_matches(".jar");
            let dir = root.join("dex").join(stem);
            let mut files: Vec<PathBuf> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.starts_with("classes") && name.ends_with(".dex") {
                        files.push(e.path());
                    }
                }
            }
            files.sort_by_key(|p| {
                dex_number(&p.file_name().unwrap_or_default().to_string_lossy())
            });
            for path in files {
                let bytes = std::fs::read(&path)?;
                let dex = DexFile::parse(bytes).map_err(|err| GenError::Dex {
                    path: path.display().to_string(),
                    err,
                })?;
                dexes.push(LoadedDex {
                    jar: jar.clone(),
                    path,
                    dex,
                });
            }
        }
        Ok(BootScanner { root, jars, dexes })
    }

    /// Total number of loaded DEX files.
    pub fn dex_count(&self) -> usize {
        self.dexes.len()
    }

    /// Every `$Stub` class in boot order, as `(dex index, stub descriptor)`.
    pub fn stub_descriptors(&self) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        for (i, d) in self.dexes.iter().enumerate() {
            for desc in crate::decode::stub_classes(&d.dex) {
                out.push((i, desc));
            }
        }
        out
    }

    /// Find the stub/native class for a dotted interface name.
    pub fn find_stub(&self, dotted_name: &str) -> Option<(usize, String)> {
        let stub_desc = if let Some((native, _)) = crate::decode::handwritten_pair(dotted_name) {
            native.to_string()
        } else {
            let d = descriptor_of(dotted_name);
            format!("{}$Stub;", d.trim_end_matches(';'))
        };
        for (i, loaded) in self.dexes.iter().enumerate() {
            if loaded.dex.find_class(&stub_desc).is_some() {
                return Some((i, stub_desc));
            }
        }
        None
    }

    /// Decode the interface whose stub/native descriptor is `stub_desc`.
    pub fn decode_stub(
        &self,
        dex_idx: usize,
        stub_desc: &str,
    ) -> Result<InterfaceInfo, DecodeError> {
        let dex = &self.dexes[dex_idx].dex;
        let def = dex
            .find_class(stub_desc)
            .ok_or_else(|| DecodeError::NoClass(stub_desc.to_string()))?;
        let interface = crate::decode::interface_for_stub(stub_desc)
            .ok_or_else(|| DecodeError::NoClass(stub_desc.to_string()))?;
        let proxy = crate::decode::proxy_for_stub(stub_desc);
        Scanner::new(dex).decode_pair(def, stub_desc, &proxy, &interface)
    }

    /// Decode a dotted interface name if it is present.
    pub fn decode_interface(&self, dotted_name: &str) -> Option<InterfaceInfo> {
        let (i, stub) = self.find_stub(dotted_name)?;
        self.decode_stub(i, &stub).ok()
    }

    /// Decode every `$Stub` in the boot classpath, sorted by descriptor.
    pub fn decode_all(&self) -> (Vec<InterfaceInfo>, Vec<(String, String)>) {
        let mut out = Vec::new();
        let mut errors = Vec::new();
        let mut seen = BTreeSet::new();
        for (i, stub_desc) in self.stub_descriptors() {
            if !seen.insert(stub_desc.clone()) {
                continue;
            }
            match self.decode_stub(i, &stub_desc) {
                Ok(info) => out.push(info),
                Err(e) => errors.push((stub_desc, e.to_string())),
            }
        }
        out.sort_by(|a, b| a.descriptor.cmp(&b.descriptor));
        (out, errors)
    }
}

/// One row of the measured work-lists.
#[derive(Clone, Debug)]
pub struct SubsetRow {
    pub source: String,
    pub interface: String,
    pub method: String,
    pub kind: String,
    pub signature: String,
}

impl SubsetRow {
    /// The AIDL method name this row refers to. For `handler` rows the method
    /// column is `<HandlerClass>.<method>`; the last segment is the AIDL name.
    pub fn lookup_name(&self) -> &str {
        match self.kind.as_str() {
            "handler" => self.method.rsplit('.').next().unwrap_or(&self.method),
            _ => &self.method,
        }
    }

    /// `true` when this row is an incoming `onTransact` pseudo-row, which has no
    /// AIDL method of its own (only the interface is checked).
    pub fn is_on_transact(&self) -> bool {
        self.kind == "onTransact"
    }
}

fn split_tsv(line: &str) -> Vec<String> {
    line.split('\t').map(str::to_string).collect()
}

fn read_tsv_rows(path: &Path, source: &str, services: bool) -> Result<Vec<SubsetRow>, GenError> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let cols = split_tsv(line);
        if services {
            if cols.len() < 5 {
                continue;
            }
            out.push(SubsetRow {
                source: source.to_string(),
                interface: cols[0].clone(),
                method: cols[3].clone(),
                kind: "services".to_string(),
                signature: cols[4].clone(),
            });
        } else {
            if cols.len() < 7 {
                continue;
            }
            out.push(SubsetRow {
                source: source.to_string(),
                interface: cols[2].clone(),
                method: cols[3].clone(),
                kind: cols[6].clone(),
                signature: cols[4].clone(),
            });
        }
    }
    Ok(out)
}

/// Read every measured work-list row (services.tsv + the two binder traces).
pub fn read_subset_rows(workspace: &Path) -> Result<Vec<SubsetRow>, GenError> {
    let mut rows = Vec::new();
    let services = workspace.join("research/service-layer/services.tsv");
    rows.extend(read_tsv_rows(&services, "services.tsv", true)?);
    for (name, path) in [
        (
            "binder_calls_launch.tsv",
            workspace.join("research/dynamic-trace/data/binder_calls_launch.tsv"),
        ),
        (
            "binder_calls_interaction.tsv",
            workspace.join("research/dynamic-trace/data/binder_calls_interaction.tsv"),
        ),
    ] {
        rows.extend(read_tsv_rows(&path, name, false)?);
    }
    Ok(rows)
}

/// Interfaces referenced by the measured work-lists (sorted).
pub fn subset_interfaces(rows: &[SubsetRow]) -> BTreeSet<String> {
    rows.iter().map(|r| r.interface.clone()).collect()
}

/// `(interface -> AIDL method names)` referenced by the measured work-lists.
pub fn subset_methods(rows: &[SubsetRow]) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for r in rows {
        out.entry(r.interface.clone())
            .or_default()
            .insert(r.lookup_name().to_string());
    }
    out
}

/// Decode the full default subset (every interface referenced by the measured
/// work-lists) into a descriptor-sorted vector.
pub fn scan_default(
    scanner: &BootScanner,
    rows: &[SubsetRow],
) -> (Vec<InterfaceInfo>, Vec<(String, String)>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for iface in subset_interfaces(rows) {
        match scanner.decode_interface(&iface) {
            Some(info) => out.push(info),
            None => errors.push((iface.clone(), "no $Stub in boot classpath".to_string())),
        }
    }
    out.sort_by(|a, b| a.descriptor.cmp(&b.descriptor));
    (out, errors)
}

/// A method whose layout contains at least one `Unknown`.
#[derive(Clone, Debug)]
pub struct UnknownCase {
    pub interface: String,
    pub method: String,
    pub code: u32,
    pub reason: String,
}

/// Coverage of a decoded interface set.
#[derive(Clone, Debug)]
pub struct Coverage {
    pub interfaces: usize,
    pub methods: usize,
    pub fully_decoded: usize,
    pub unknown: Vec<UnknownCase>,
}

impl Coverage {
    /// Percentage of methods fully decoded.
    pub fn percent(&self) -> f64 {
        if self.methods == 0 {
            100.0
        } else {
            self.fully_decoded as f64 * 100.0 / self.methods as f64
        }
    }
}

fn first_reason(m: &MethodInfo) -> String {
    if let Some(inc) = &m.incomplete {
        return inc.clone();
    }
    for (side, ops) in [("args", &m.args), ("reply", &m.reply)] {
        if let Some(op) = ops.iter().find(|o| o.is_unknown()) {
            return format!("{side}: {}", op.render());
        }
    }
    "unknown".to_string()
}

/// Compute coverage. When `only` is provided, restrict to those method names
/// per interface (the measured default subset); otherwise cover everything.
pub fn coverage(
    ifaces: &[InterfaceInfo],
    only: Option<&BTreeMap<String, BTreeSet<String>>>,
) -> Coverage {
    let mut cov = Coverage {
        interfaces: 0,
        methods: 0,
        fully_decoded: 0,
        unknown: Vec::new(),
    };
    for iface in ifaces {
        let mut counted_interface = false;
        for m in &iface.methods {
            if let Some(filter) = only {
                match filter.get(&iface.descriptor) {
                    Some(set) if set.contains(&m.name) => {}
                    _ => continue,
                }
            }
            counted_interface = true;
            cov.methods += 1;
            if m.fully_decoded() {
                cov.fully_decoded += 1;
            } else {
                cov.unknown.push(UnknownCase {
                    interface: iface.descriptor.clone(),
                    method: m.name.clone(),
                    code: m.code,
                    reason: first_reason(m),
                });
            }
        }
        if counted_interface {
            cov.interfaces += 1;
        }
    }
    cov
}

fn render_op(op: &WireOp) -> String {
    match op {
        WireOp::I32 => "WireOp::I32".into(),
        WireOp::I64 => "WireOp::I64".into(),
        WireOp::F32 => "WireOp::F32".into(),
        WireOp::F64 => "WireOp::F64".into(),
        WireOp::Bool => "WireOp::Bool".into(),
        WireOp::Byte => "WireOp::Byte".into(),
        WireOp::Char => "WireOp::Char".into(),
        WireOp::Str16 => "WireOp::Str16".into(),
        WireOp::Str8 => "WireOp::Str8".into(),
        WireOp::Binder { iface: None } => "super::binder(None)".into(),
        WireOp::Binder { iface: Some(i) } => format!("super::binder(Some({i:?}))"),
        WireOp::ByteArray => "WireOp::ByteArray".into(),
        WireOp::IntArray => "WireOp::IntArray".into(),
        WireOp::LongArray => "WireOp::LongArray".into(),
        WireOp::FloatArray => "WireOp::FloatArray".into(),
        WireOp::DoubleArray => "WireOp::DoubleArray".into(),
        WireOp::BooleanArray => "WireOp::BooleanArray".into(),
        WireOp::CharArray => "WireOp::CharArray".into(),
        WireOp::StringArray => "WireOp::StringArray".into(),
        WireOp::StringList => "WireOp::StringList".into(),
        WireOp::BinderList => "WireOp::BinderList".into(),
        WireOp::RawFd => "WireOp::RawFd".into(),
        WireOp::TypedArray { class } => format!("super::typed_array({class:?})"),
        WireOp::TypedList { class } => format!("super::typed_list({class:?})"),
        WireOp::Parcelable { class } => format!("super::parcelable({class:?})"),
        WireOp::TypedObject {
            class,
            nullable: true,
        } => format!("super::typed_object({class:?})"),
        WireOp::TypedObject {
            class,
            nullable: false,
        } => format!("super::typed_object_nn({class:?})"),
        WireOp::List { elem } => format!("super::list_op({})", render_op(elem)),
        WireOp::Array { elem } => format!("super::array_op({})", render_op(elem)),
        WireOp::Map { key, value } => format!(
            "super::map_op({}, {})",
            render_op(key),
            render_op(value)
        ),
        WireOp::Bundle => "WireOp::Bundle".into(),
        WireOp::Value => "super::value_op()".into(),
        WireOp::Cond { marker, ops } => {
            let inner: Vec<String> = ops.iter().map(render_op).collect();
            format!(
                "super::cond({}, vec![{}])",
                render_op(marker),
                inner.join(", ")
            )
        }
        WireOp::Unknown { note } => format!("super::unknown({note:?})"),
    }
}

fn render_ops(ops: &[WireOp], indent: &str) -> String {
    if ops.is_empty() {
        return "vec![]".to_string();
    }
    if ops.len() <= 3 {
        let inner: Vec<String> = ops.iter().map(render_op).collect();
        return format!("vec![{}]", inner.join(", "));
    }
    let mut s = String::from("vec![\n");
    for op in ops {
        s.push_str(indent);
        s.push_str("    ");
        s.push_str(&render_op(op));
        s.push_str(",\n");
    }
    s.push_str(indent);
    s.push(']');
    s
}

/// Render the deterministic `tables.rs` source for a set of interfaces.
pub fn render_tables(ifaces: &[InterfaceInfo]) -> String {
    let mut sorted: Vec<&InterfaceInfo> = ifaces.iter().collect();
    sorted.sort_by(|a, b| a.descriptor.cmp(&b.descriptor));

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo run -p acos-aidl --example gen -- --boot .cache/boot/android-34`\n",
    );
    out.push_str("//! Default ACOS AIDL subset (regenerated by the `gen` example).\n");
    out.push_str("//!\n");
    out.push_str("//! Do not edit by hand: regenerate and diff instead.\n\n");
    out.push_str("use crate::ir::WireOp;\n");
    out.push_str("use std::sync::LazyLock;\n\n");
    out.push_str("/// Every AIDL interface in the default ACOS service-layer subset.\n");
    out.push_str("pub static INTERFACES: LazyLock<Vec<crate::ir::InterfaceInfo>> = LazyLock::new(|| {\n");
    out.push_str("    vec![\n");
    for iface in sorted {
        out.push_str(&format!(
            "        crate::ir::InterfaceInfo {{\n            descriptor: {:?}.to_string(),\n            first_call_transaction: {},\n            methods: vec![\n",
            iface.descriptor, iface.first_call_transaction
        ));
        for m in &iface.methods {
            out.push_str(&format!(
                "                crate::ir::MethodInfo {{\n                    name: {:?}.to_string(),\n                    signature: {:?}.to_string(),\n                    return_type: {:?}.to_string(),\n                    code: {},\n                    oneway: {},\n                    args: {},\n                    reply: {},\n                    incomplete: None,\n                }},\n",
                m.name,
                m.signature,
                m.return_type,
                m.code,
                m.oneway,
                render_ops(&m.args, "                    "),
                render_ops(&m.reply, "                    "),
            ));
        }
        out.push_str("            ],\n        },\n");
    }
    out.push_str("    ]\n});\n");
    out
}

/// Serialise a full-boot scan as pretty JSON (deterministic ordering).
pub fn render_json(ifaces: &[InterfaceInfo]) -> Result<String, GenError> {
    let mut sorted: Vec<&InterfaceInfo> = ifaces.iter().collect();
    sorted.sort_by(|a, b| a.descriptor.cmp(&b.descriptor));
    Ok(serde_json::to_string_pretty(&sorted)?)
}
