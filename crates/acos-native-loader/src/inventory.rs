//! Static import inventory for guest shared objects.
//!
//! This module never maps or executes anything: it parses an ELF image the same
//! way the loader does (`elf` + `dyninfo`) and reports what a real Android
//! library needs, then classifies every undefined import against the bionic
//! compatibility table as `provided`, `partial` or `missing`.
//!
//! Used by `acos native-inventory <apk-or-so...>` and by the checked-in
//! corpus inventory under `results/native-loader/inventory/`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::bionic;
use crate::dyninfo::DynInfo;
use crate::elf::*;

/// How well a guest import is satisfied today.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provision {
    /// Resolved by a complete bionic-table implementation.
    Provided,
    /// Resolved by a shim that is known to be incomplete/approximate.
    Partial,
    /// Absent from the bionic table (Strict load fails / LazyTrap aborts).
    Missing,
}

impl Provision {
    pub fn as_str(self) -> &'static str {
        match self {
            Provision::Provided => "provided",
            Provision::Partial => "partial",
            Provision::Missing => "missing",
        }
    }
}

/// Symbols the bionic table resolves but only approximately. Kept explicit so
/// the inventory cannot claim more than the loader delivers.
pub const PARTIAL_SYMBOLS: &[(&str, &str)] = &[
    ("dladdr", "reports the object but not the nearest symbol"),
    ("dlopen", "search-path confined, no RTLD_GLOBAL/LOCAL semantics"),
    ("dlsym", "RTLD_NEXT unsupported"),
    ("dlclose", "unloads on last handle only"),
    ("dlerror", "always NULL (no error text)"),
    ("android_dlopen_ext", "extinfo now honoured for fd+offset; other flags ignored"),
    ("dl_iterate_phdr", "guest objects only, no host objects"),
    ("__system_property_find", "returns NULL"),
    ("__system_property_read_callback", "no-op"),
    ("__system_property_wait", "no-op"),
    ("__system_property_serial", "returns 0"),
    ("__system_property_area_serial", "returns 0"),
    ("getauxval", "host auxv, not Android's"),
    ("pthread_getattr_np", "reports the host stack, not the guest one"),
    ("__cxa_thread_atexit_impl", "runs on host thread exit only"),
];

/// Relocation type names (x86_64) used in the report.
pub fn reloc_name(t: u32) -> &'static str {
    match t {
        R_X86_64_NONE => "NONE",
        R_X86_64_64 => "64",
        R_X86_64_PC32 => "PC32",
        3 => "GOT32",
        4 => "PLT32",
        R_X86_64_COPY => "COPY",
        R_X86_64_GLOB_DAT => "GLOB_DAT",
        R_X86_64_JUMP_SLOT => "JUMP_SLOT",
        R_X86_64_RELATIVE => "RELATIVE",
        9 => "GOTPCREL",
        R_X86_64_32 => "32",
        R_X86_64_32S => "32S",
        12 => "16",
        13 => "PC16",
        14 => "8",
        15 => "PC8",
        R_X86_64_TLS_DTPMOD => "TLS_DTPMOD64",
        R_X86_64_TLS_DTPOFF => "TLS_DTPOFF64",
        R_X86_64_TLS_TPOFF => "TLS_TPOFF64",
        19 => "TLS_GD",
        20 => "TLS_LD",
        21 => "TLS_GD32",
        22 => "TLS_GD32S",
        24 => "TLS_LDM",
        25 => "TLS_LD32",
        26 => "TLS_LD32S",
        27 => "TLS_GD64",
        28 => "TLS_LD64",
        29 => "TLS_GOTPC32",
        30 => "TLS_GOTPC32S",
        31 => "TLS_GOTPC64",
        32 => "TLS_GOTIE64",
        33 => "TLS_LDM64",
        34 => "TLS_IE64",
        35 => "TLS_IE32S",
        36 => "TLSDESC",
        37 => "IRELATIVE",
        38 => "RELATIVE64",
        41 => "GOTPCRELX",
        42 => "REX_GOTPCRELX",
        43 => "TLS_GOTPC32X",
        _ => "UNKNOWN",
    }
}

/// TLS addressing model inferred from the relocation mix.
#[derive(Debug, Default, Clone, Serialize)]
pub struct TlsModels {
    /// `__tls_get_addr` general-dynamic (DTPMOD64/DTPOFF64/TLS_GD*).
    pub general_dynamic: bool,
    /// initial-exec (TLS_TPOFF64 / TLS_IE*).
    pub initial_exec: bool,
    /// TLSDESC descriptor relocations.
    pub tlsdesc: bool,
    /// Number of `STT_TLS` definitions.
    pub defined_tls_symbols: usize,
    /// `PT_TLS` geometry, if present.
    pub segment: Option<TlsSegmentReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TlsSegmentReport {
    pub filesz: u64,
    pub memsz: u64,
    pub align: u64,
}

/// One undefined import.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Import {
    pub name: String,
    /// Requested symbol version (`GLIBC_*`, `LIBC`, `...`), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub binding: String,
    pub sym_type: String,
    pub ifunc: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LibraryInventory {
    pub path: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soname: Option<String>,
    pub machine: u16,
    pub abi: String,
    pub needed: Vec<String>,
    pub relocations: RelocReport,
    pub tls: TlsModels,
    pub init: InitReport,
    pub imports: Vec<Import>,
    /// Exported (defined, global/weak, non-empty) symbol names.
    pub export_names: Vec<String>,
    /// Exported (defined, global/weak) symbol count.
    pub exports: usize,
    /// Defined `STT_GNU_IFUNC` symbols.
    pub ifuncs: Vec<String>,
    /// Imports grouped by the Android library that traditionally provides them.
    pub import_count: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RelocReport {
    pub total: usize,
    pub by_type: BTreeMap<String, usize>,
    /// Relocation container formats present.
    pub containers: Vec<String>,
    /// Number of APS2 packed groups (0 = no APS2 section).
    pub aps2_groups: usize,
    /// Multi-group APS2 is what the loader used to reject.
    pub aps2_multi_group: bool,
    pub textrel: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct InitReport {
    pub dt_init: bool,
    pub dt_fini: bool,
    pub init_array_count: usize,
    pub fini_array_count: usize,
    pub preinit_array_count: usize,
}

/// Reasons a library is not loadable today, derived from the inventory.
#[derive(Debug, Clone, Serialize)]
pub struct Blockers {
    pub missing_symbols: Vec<String>,
    pub initial_exec_tls: bool,
    pub aps2_multi_group: bool,
    pub ifuncs: Vec<String>,
    pub unsupported_relocations: Vec<String>,
}

/// Aggregate report over a library set.
#[derive(Debug, Clone, Serialize)]
pub struct InventoryReport {
    pub abi: String,
    pub libraries: Vec<LibraryInventory>,
    /// Undefined symbol -> set of libraries that need it.
    pub unresolved: Vec<SymbolRow>,
    pub totals: Totals,
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolRow {
    pub symbol: String,
    pub provision: Provision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub libraries: Vec<String>,
    /// Total relocation sites across the corpus.
    pub sites: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub libraries: usize,
    pub imports: usize,
    pub provided: usize,
    pub partial: usize,
    pub missing: usize,
    pub defined_in_corpus: usize,
}

fn sym_type_name(t: u8) -> &'static str {
    match t {
        STT_NOTYPE => "NOTYPE",
        STT_OBJECT => "OBJECT",
        STT_FUNC => "FUNC",
        STT_SECTION => "SECTION",
        STT_FILE => "FILE",
        STT_COMMON => "COMMON",
        STT_TLS => "TLS",
        10 => "GNU_IFUNC",
        _ => "OTHER",
    }
}

fn bind_name(b: u8) -> &'static str {
    match b {
        STB_LOCAL => "LOCAL",
        STB_GLOBAL => "GLOBAL",
        STB_WEAK => "WEAK",
        10 => "GNU_UNIQUE",
        _ => "OTHER",
    }
}

/// One decoded relocation.
#[derive(Debug, Clone, Copy)]
pub struct RawReloc {
    pub offset: u64,
    pub r_type: u32,
    pub sym: u32,
    pub addend: i64,
}

fn leb128(data: &[u8], cur: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        let b = *data.get(*cur)?;
        *cur += 1;
        result |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

fn sleb128(data: &[u8], cur: &mut usize) -> Option<i64> {
    let mut result: i64 = 0;
    let mut shift = 0u32;
    loop {
        let b = *data.get(*cur)?;
        *cur += 1;
        result |= ((b & 0x7f) as i64) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            if shift < 64 && b & 0x40 != 0 {
                result |= -1i64 << shift;
            }
            return Some(result);
        }
        if shift > 63 {
            return None;
        }
    }
}

fn section<'a>(elf: &Elf<'a>, va: u64, size: u64) -> Option<&'a [u8]> {
    let off = elf.vaddr_to_offset(va)?;
    elf.data.get(off..off.checked_add(size as usize)?)
}

/// Decode RELA/REL/RELR/APS2 into a flat relocation list.
pub fn decode_relocations(
    elf: &Elf<'_>,
    info: &DynInfo,
) -> (Vec<RawReloc>, RelocReport) {
    let mut out = Vec::new();
    let mut report = RelocReport {
        textrel: info.textrel,
        ..Default::default()
    };
    let mut containers: BTreeSet<&'static str> = BTreeSet::new();

    if let Some((va, sz)) = info.rela {
        if let Some(d) = section(elf, va, sz) {
            containers.insert("RELA");
            for c in d.chunks_exact(24) {
                let offset = u64::from_le_bytes(c[0..8].try_into().unwrap());
                let r_info = u64::from_le_bytes(c[8..16].try_into().unwrap());
                let addend = i64::from_le_bytes(c[16..24].try_into().unwrap());
                out.push(RawReloc {
                    offset,
                    r_type: (r_info & 0xffff_ffff) as u32,
                    sym: (r_info >> 32) as u32,
                    addend,
                });
            }
        }
    }
    if let Some((va, sz)) = info.rel {
        if let Some(d) = section(elf, va, sz) {
            containers.insert("REL");
            for c in d.chunks_exact(16) {
                let offset = u64::from_le_bytes(c[0..8].try_into().unwrap());
                let r_info = u64::from_le_bytes(c[8..16].try_into().unwrap());
                out.push(RawReloc {
                    offset,
                    r_type: (r_info & 0xffff_ffff) as u32,
                    sym: (r_info >> 32) as u32,
                    addend: 0,
                });
            }
        }
    }
    if let Some((va, sz)) = info.jmprel {
        if let Some(d) = section(elf, va, sz) {
            containers.insert(if info.jmprel_is_rela { "JMPREL-RELA" } else { "JMPREL-REL" });
            let entsz = if info.jmprel_is_rela { 24 } else { 16 };
            for c in d.chunks_exact(entsz) {
                let offset = u64::from_le_bytes(c[0..8].try_into().unwrap());
                let r_info = u64::from_le_bytes(c[8..16].try_into().unwrap());
                let addend = if entsz == 24 {
                    i64::from_le_bytes(c[16..24].try_into().unwrap())
                } else {
                    0
                };
                out.push(RawReloc {
                    offset,
                    r_type: (r_info & 0xffff_ffff) as u32,
                    sym: (r_info >> 32) as u32,
                    addend,
                });
            }
        }
    }
    if let Some((va, sz, android)) = info.relr {
        if let Some(d) = section(elf, va, sz) {
            containers.insert(if android { "ANDROID_RELR" } else { "RELR" });
            let mut where_: u64 = 0;
            for c in d.chunks_exact(8) {
                let e = u64::from_le_bytes(c.try_into().unwrap());
                if e & 1 == 0 {
                    where_ = e;
                    out.push(RawReloc {
                        offset: where_,
                        r_type: R_X86_64_RELATIVE,
                        sym: 0,
                        addend: 0,
                    });
                    where_ = where_.wrapping_add(8);
                } else {
                    let bits = e >> 1;
                    for b in 0..63u64 {
                        if bits & (1 << b) != 0 {
                            out.push(RawReloc {
                                offset: where_.wrapping_add(b * 8),
                                r_type: R_X86_64_RELATIVE,
                                sym: 0,
                                addend: 0,
                            });
                        }
                    }
                    where_ = where_.wrapping_add(63 * 8);
                }
            }
        }
    }
    if let Some((va, sz, has_addend)) = info.android_packed {
        if let Some(d) = section(elf, va, sz) {
            containers.insert("ANDROID_APS2");
            let mut cur = 0usize;
            if d.len() >= 4 && &d[0..4] == b"APS2" {
                cur = 4;
            }
            let mut groups = 0usize;
            let mut remaining = leb128(d, &mut cur).unwrap_or(0) as usize;
            let _reserved = leb128(d, &mut cur).unwrap_or(0);
            while let Some(v) = leb128(d, &mut cur) {
                let group_size = v as usize;
                let _flags = leb128(d, &mut cur).unwrap_or(0);
                let info_word = leb128(d, &mut cur).unwrap_or(0);
                let r_type = (info_word & 0xffff_ffff) as u32;
                groups += 1;
                let mut offset: i64 = 0;
                let mut addend: i64 = 0;
                for i in 0..group_size {
                    let ok = if i == 0 {
                        match sleb128(d, &mut cur) {
                            Some(o) => {
                                offset = o;
                                true
                            }
                            None => false,
                        }
                    } else {
                        match leb128(d, &mut cur) {
                            Some(delta) => {
                                offset += delta as i64;
                                true
                            }
                            None => false,
                        }
                    };
                    if !ok {
                        break;
                    }
                    if has_addend {
                        match sleb128(d, &mut cur) {
                            Some(a) if i == 0 => addend = a,
                            Some(da) => addend += da,
                            None => break,
                        }
                    }
                    out.push(RawReloc {
                        offset: offset as u64,
                        r_type,
                        sym: 0,
                        addend,
                    });
                }
                remaining = remaining.saturating_sub(group_size);
                if remaining == 0 || cur >= d.len() {
                    break;
                }
            }
            report.aps2_groups = groups;
            report.aps2_multi_group = groups > 1;
        }
    }
    report.containers = containers.into_iter().map(str::to_string).collect();
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    for r in &out {
        *by_type.entry(reloc_name(r.r_type).to_string()).or_default() += 1;
    }
    report.total = out.len();
    report.by_type = by_type;
    (out, report)
}

/// Inventory one ELF image.
pub fn inventory_bytes(path: &str, name: &str, bytes: &[u8]) -> Result<LibraryInventory, ElfError> {
    let elf = Elf::parse(bytes)?;
    let info = DynInfo::parse(&elf)?;
    let (_relocs, relocations) = decode_relocations(&elf, &info);

    let mut imports: Vec<Import> = Vec::new();
    let mut exports = 0usize;
    let mut export_names: Vec<String> = Vec::new();
    let mut ifuncs = Vec::new();
    let mut tls_syms = 0usize;
    for (i, s) in info.symbols.iter().enumerate() {
        let Some(n) = info.symbol_name(i) else { continue };
        if n.is_empty() {
            continue;
        }
        let st = s.sym_type();
        if s.shndx == SHN_UNDEF {
            if s.bind() != STB_LOCAL {
                imports.push(Import {
                    name: n.to_string(),
                    version: info.symbol_version(i).map(str::to_string),
                    binding: bind_name(s.bind()).to_string(),
                    sym_type: sym_type_name(st).to_string(),
                    ifunc: st == 10,
                });
            }
        } else if s.bind() != STB_LOCAL {
            exports += 1;
            export_names.push(n.to_string());
            if st == 10 {
                ifuncs.push(n.to_string());
            }
            if st == STT_TLS {
                tls_syms += 1;
            }
        }
    }
    imports.sort();
    imports.dedup();

    let has = |t: u32| relocations.by_type.contains_key(reloc_name(t));
    let tls = TlsModels {
        general_dynamic: has(R_X86_64_TLS_DTPMOD)
            || has(R_X86_64_TLS_DTPOFF)
            || relocations
                .by_type
                .keys()
                .any(|k| k.starts_with("TLS_GD")),
        initial_exec: has(R_X86_64_TLS_TPOFF)
            || relocations
                .by_type
                .keys()
                .any(|k| k.starts_with("TLS_IE") || k == "TLS_GOTIE64"),
        tlsdesc: has(R_X86_64_TLSDESC),
        defined_tls_symbols: tls_syms,
        segment: info.tls.map(|t| TlsSegmentReport {
            filesz: t.filesz,
            memsz: t.memsz,
            align: t.align,
        }),
    };

    let count_ptrs = |arr: Option<(u64, u64)>| -> usize {
        match arr {
            Some((_, sz)) => (sz as usize) / 8,
            None => 0,
        }
    };

    Ok(LibraryInventory {
        path: path.to_string(),
        name: name.to_string(),
        soname: info.soname.clone(),
        machine: elf.header.e_machine,
        abi: if elf.header.e_machine == EM_X86_64 {
            "x86_64".into()
        } else {
            format!("machine-{}", elf.header.e_machine)
        },
        needed: info.needed.clone(),
        relocations,
        tls,
        init: InitReport {
            dt_init: info.init != 0,
            dt_fini: info.fini != 0,
            init_array_count: count_ptrs(info.init_array),
            fini_array_count: count_ptrs(info.fini_array),
            preinit_array_count: count_ptrs(info.preinit_array),
        },
        import_count: imports.len(),
        imports,
        exports,
        export_names,
        ifuncs,
    })
}

/// Inventory a `.so` file on disk.
pub fn inventory_file(path: &Path) -> Result<LibraryInventory, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    inventory_bytes(&path.display().to_string(), &name, &bytes).map_err(|e| e.0)
}

fn partial_note(name: &str) -> Option<String> {
    PARTIAL_SYMBOLS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, why)| (*why).to_string())
}

/// Classify a symbol name against the bionic table.
pub fn classify(name: &str) -> Provision {
    if partial_note(name).is_some() {
        return Provision::Partial;
    }
    if bionic::resolve(name).is_some() {
        Provision::Provided
    } else {
        Provision::Missing
    }
}

/// Aggregate a set of inventories into one ranked report.
///
/// Symbols defined by any library in the corpus satisfy imports of sibling
/// libraries (ELF interposition within the load group), so they are reported as
/// `provided` with the defining library listed in `note`.
pub fn aggregate(abi: &str, libs: Vec<LibraryInventory>) -> InventoryReport {
    let mut defined: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for l in &libs {
        // Exports are recomputed cheaply: names of defined dynamic symbols.
        for e in l.export_names.iter().cloned() {
            defined.entry(e).or_default().push(l.name.clone());
        }
    }
    let mut rows: BTreeMap<String, SymbolRow> = BTreeMap::new();
    for l in &libs {
        for imp in &l.imports {
            let row = rows.entry(imp.name.clone()).or_insert_with(|| {
                let provision = match defined.get(&imp.name) {
                    Some(_) => Provision::Provided,
                    None => classify(&imp.name),
                };
                SymbolRow {
                    symbol: imp.name.clone(),
                    provision,
                    version: imp.version.clone(),
                    libraries: Vec::new(),
                    sites: 0,
                    note: partial_note(&imp.name).or_else(|| {
                        defined
                            .get(&imp.name)
                            .map(|libs| format!("defined in corpus: {}", libs.join(", ")))
                    }),
                }
            });
            if !row.libraries.contains(&l.name) {
                row.libraries.push(l.name.clone());
            }
            row.sites += 1;
        }
    }
    let mut unresolved: Vec<SymbolRow> = rows.into_values().collect();
    // missing first, then by number of consuming libraries, then name
    unresolved.sort_by(|a, b| {
        b.provision
            .cmp(&a.provision)
            .then(b.libraries.len().cmp(&a.libraries.len()))
            .then(a.symbol.cmp(&b.symbol))
    });
    let mut totals = Totals {
        libraries: libs.len(),
        ..Default::default()
    };
    for r in &unresolved {
        totals.imports += 1;
        match r.provision {
            Provision::Provided => totals.provided += 1,
            Provision::Partial => totals.partial += 1,
            Provision::Missing => totals.missing += 1,
        }
    }
    totals.defined_in_corpus = defined.len();
    InventoryReport {
        abi: abi.to_string(),
        libraries: libs,
        unresolved,
        totals,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_symbols_against_the_bionic_table() {
        // Forwarded libc entry.
        assert_eq!(classify("memcpy"), Provision::Provided);
        // Added by the RN corpus inventory (previously missing).
        assert_eq!(classify("prctl"), Provision::Provided);
        assert_eq!(classify("sem_post"), Provision::Provided);
        assert_eq!(classify("stderr"), Provision::Provided);
        // Documented-but-incomplete shim.
        assert_eq!(classify("dladdr"), Provision::Partial);
        // Not provided at all: must stay missing (never silently stubbed).
        assert_eq!(classify("AndroidBitmap_lockPixels"), Provision::Missing);
        assert_eq!(classify("acos_definitely_missing_symbol"), Provision::Missing);
    }

    #[test]
    fn reloc_type_names_cover_the_corpus_set() {
        assert_eq!(reloc_name(R_X86_64_JUMP_SLOT), "JUMP_SLOT");
        assert_eq!(reloc_name(R_X86_64_TLS_DTPMOD), "TLS_DTPMOD64");
        assert_eq!(reloc_name(R_X86_64_TLSDESC), "TLSDESC");
        assert_eq!(reloc_name(R_X86_64_IRELATIVE), "IRELATIVE");
    }

    #[test]
    fn rejects_non_elf_input() {
        let err = inventory_bytes("x.so", "x.so", b"not an elf at all");
        assert!(err.is_err());
    }
}
