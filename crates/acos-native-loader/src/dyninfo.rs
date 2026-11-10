//! Dynamic-section parsing: symbol tables, GNU/SysV hash lookup, symbol
//! versioning, relocation descriptor locations, TLS and init/fini metadata.

use crate::elf::*;

/// Errors from dynamic parsing reuse [`ElfError`].
pub type DynResult<T> = std::result::Result<T, ElfError>;

fn derr<T>(msg: impl Into<String>) -> DynResult<T> {
    Err(ElfError(msg.into()))
}

/// GNU hash table, parsed into vectors.
pub struct GnuHash {
    pub nbuckets: u32,
    pub symoffset: u32,
    pub bloom_shift: u32,
    pub bloom: Vec<u64>,
    pub buckets: Vec<u32>,
    pub chain: Vec<u32>,
}

impl GnuHash {
    /// Compute the GNU symbol hash.
    pub fn hash(name: &[u8]) -> u32 {
        let mut h: u32 = 5381;
        for &c in name {
            h = h.wrapping_mul(33).wrapping_add(c as u32);
        }
        h
    }

    /// Look up `name`; returns the symbol index on success.
    pub fn lookup(&self, name: &[u8]) -> Option<u32> {
        if self.buckets.is_empty() || self.bloom.is_empty() {
            return None;
        }
        let h = Self::hash(name);
        let word = *self.bloom.get(((h as usize) / 64) % self.bloom.len())?;
        let bit1 = 1u64 << (h % 64);
        let bit2 = 1u64 << ((h >> self.bloom_shift) % 64);
        if word & (bit1 | bit2) != (bit1 | bit2) {
            return None;
        }
        let mut idx = *self.buckets.get((h as usize) % self.buckets.len())? as usize;
        if (idx as u32) < self.symoffset {
            return None;
        }
        loop {
            let chain_idx = idx.checked_sub(self.symoffset as usize)?;
            let c = *self.chain.get(chain_idx)?;
            if (c | 1) == (h | 1) {
                return Some(idx as u32);
            }
            if c & 1 != 0 {
                return None;
            }
            idx += 1;
        }
    }

    /// Total number of dynamic symbols implied by this hash table.
    pub fn symbol_count(&self) -> usize {
        let mut max = self.symoffset as usize;
        for &b in &self.buckets {
            let mut idx = b as usize;
            if idx < self.symoffset as usize {
                continue;
            }
            loop {
                let ci = idx - self.symoffset as usize;
                match self.chain.get(ci) {
                    None => break,
                    Some(&c) => {
                        max = max.max(idx + 1);
                        if c & 1 != 0 {
                            break;
                        }
                        idx += 1;
                    }
                }
            }
        }
        max
    }
}

/// Classic SysV hash table.
pub struct SysvHash {
    pub buckets: Vec<u32>,
    pub chains: Vec<u32>,
}

impl SysvHash {
    fn hash(name: &[u8]) -> u32 {
        let mut h: u32 = 0;
        for &c in name {
            h = h.wrapping_mul(16).wrapping_add(c as u32);
            let g = h & 0xf000_0000;
            if g != 0 {
                h ^= g >> 24;
            }
        }
        h & 0x0fff_ffff
    }

    pub fn lookup(&self, name: &[u8]) -> Option<u32> {
        if self.buckets.is_empty() {
            return None;
        }
        let h = Self::hash(name);
        let mut idx = *self.buckets.get((h as usize) % self.buckets.len())? as usize;
        while idx != 0 {
            if *self.chains.get(idx)? == h {
                return Some(idx as u32);
            }
            idx = *self.chains.get(idx)? as usize;
        }
        None
    }
}

/// `PT_TLS` segment geometry.
#[derive(Debug, Clone, Copy)]
pub struct TlsSegment {
    pub vaddr: u64,
    pub filesz: u64,
    pub memsz: u64,
    pub align: u64,
}

/// Everything the loader needs about a library's dynamic segment, resolved to
/// file offsets / virtual addresses against the input image.
pub struct DynInfo {
    pub needed: Vec<String>,
    pub soname: Option<String>,
    pub rpath: Option<String>,
    pub runpath: Option<String>,

    pub symtab_off: usize,
    pub strtab_off: usize,
    pub symbols: Vec<Sym>,
    pub strtab: Vec<u8>,

    pub gnu_hash: Option<GnuHash>,
    pub sysv_hash: Option<SysvHash>,

    /// Parsed `.gnu.version` entries (one per dynamic symbol).
    pub versym: Vec<u16>,
    /// version index -> version string, from `.gnu.version_r`.
    pub version_names: Vec<(u16, String)>,

    /// `(vaddr, byte_size)`.
    pub rela: Option<(u64, u64)>,
    pub rel: Option<(u64, u64)>,
    pub jmprel: Option<(u64, u64)>,
    pub jmprel_is_rela: bool,
    /// `(vaddr, byte_size, is_android_packed)`.
    pub relr: Option<(u64, u64, bool)>,
    /// Android APS2 packed REL/RELA: `(vaddr, byte_size, has_addend)`.
    pub android_packed: Option<(u64, u64, bool)>,

    pub pltgot: u64,
    pub init: u64,
    pub fini: u64,
    pub init_array: Option<(u64, u64)>,
    pub fini_array: Option<(u64, u64)>,
    pub preinit_array: Option<(u64, u64)>,

    pub flags: u64,
    pub flags1: u64,
    pub bind_now: bool,
    pub textrel: bool,
    pub rela_count: usize,

    pub tls: Option<TlsSegment>,
}

impl DynInfo {
    /// Parse the dynamic segment of an already-parsed [`Elf`].
    pub fn parse(elf: &Elf<'_>) -> DynResult<DynInfo> {
        let dyns = elf.dynamic_entries()?;

        let mut symtab_va = 0u64;
        let mut strtab_va = 0u64;
        let mut strsz = 0u64;
        let mut hash_va = 0u64;
        let mut gnu_hash_va = 0u64;
        let mut versym_va = 0u64;
        let mut verneed_va = 0u64;
        let mut verneed_num = 0u64;

        let mut rela_va: Option<u64> = None;
        let mut rela_sz = 0u64;
        let mut rel_va: Option<u64> = None;
        let mut rel_sz = 0u64;
        let mut jmprel_va: Option<u64> = None;
        let mut jmprel_sz = 0u64;
        let mut relr_va: Option<u64> = None;
        let mut relr_sz = 0u64;
        let mut android_relr = false;
        let mut android_va: Option<u64> = None;
        let mut android_sz = 0u64;
        let mut android_has_addend = false;

        let mut info = DynInfo {
            needed: Vec::new(),
            soname: None,
            rpath: None,
            runpath: None,
            symtab_off: 0,
            strtab_off: 0,
            symbols: Vec::new(),
            strtab: Vec::new(),
            gnu_hash: None,
            sysv_hash: None,
            versym: Vec::new(),
            version_names: Vec::new(),
            rela: None,
            rel: None,
            jmprel: None,
            jmprel_is_rela: true,
            relr: None,
            android_packed: None,
            pltgot: 0,
            init: 0,
            fini: 0,
            init_array: None,
            fini_array: None,
            preinit_array: None,
            flags: 0,
            flags1: 0,
            bind_now: false,
            textrel: false,
            rela_count: 0,
            tls: None,
        };

        for d in &dyns {
            match d.tag {
                DT_SYMTAB => symtab_va = d.val,
                DT_STRTAB => strtab_va = d.val,
                DT_STRSZ => strsz = d.val,
                DT_HASH => hash_va = d.val,
                DT_GNU_HASH => gnu_hash_va = d.val,
                DT_VERSYM => versym_va = d.val,
                DT_VERNEED => verneed_va = d.val,
                DT_VERNEEDNUM => verneed_num = d.val,
                DT_RELA => rela_va = Some(d.val),
                DT_RELASZ => rela_sz = d.val,
                DT_REL => rel_va = Some(d.val),
                DT_RELSZ => rel_sz = d.val,
                DT_JMPREL => jmprel_va = Some(d.val),
                DT_PLTRELSZ => jmprel_sz = d.val,
                DT_PLTREL => info.jmprel_is_rela = d.val == DT_RELA as u64,
                DT_RELR => relr_va = Some(d.val),
                DT_RELRSZ => relr_sz = d.val,
                DT_ANDROID_RELR => {
                    relr_va = Some(d.val);
                    android_relr = true;
                }
                DT_ANDROID_RELRSZ => relr_sz = d.val,
                DT_ANDROID_REL => {
                    android_va = Some(d.val);
                    android_has_addend = false;
                }
                DT_ANDROID_RELSZ => android_sz = d.val,
                DT_ANDROID_RELA => {
                    android_va = Some(d.val);
                    android_has_addend = true;
                }
                DT_ANDROID_RELASZ => android_sz = d.val,
                DT_PLTGOT => info.pltgot = d.val,
                DT_INIT => info.init = d.val,
                DT_FINI => info.fini = d.val,
                DT_INIT_ARRAY => info.init_array = Some((d.val, 0)),
                DT_INIT_ARRAYSZ => {
                    info.init_array = info.init_array.map(|(a, _)| (a, d.val));
                }
                DT_FINI_ARRAY => info.fini_array = Some((d.val, 0)),
                DT_FINI_ARRAYSZ => {
                    info.fini_array = info.fini_array.map(|(a, _)| (a, d.val));
                }
                DT_PREINIT_ARRAY => info.preinit_array = Some((d.val, 0)),
                DT_PREINIT_ARRAYSZ => {
                    info.preinit_array = info.preinit_array.map(|(a, _)| (a, d.val));
                }
                DT_FLAGS => info.flags = d.val,
                DT_FLAGS_1 => info.flags1 = d.val,
                DT_BIND_NOW => info.bind_now = true,
                DT_TEXTREL => info.textrel = true,
                DT_RELACOUNT => info.rela_count = d.val as usize,
                _ => {}
            }
        }

        if let Some(va) = rela_va {
            info.rela = Some((va, rela_sz));
        }
        if let Some(va) = rel_va {
            info.rel = Some((va, rel_sz));
        }
        if let Some(va) = jmprel_va {
            info.jmprel = Some((va, jmprel_sz));
        }
        if let Some(va) = relr_va {
            info.relr = Some((va, relr_sz, android_relr));
        }
        if let Some(va) = android_va {
            info.android_packed = Some((va, android_sz, android_has_addend));
        }

        info.bind_now = info.bind_now
            || (info.flags & 0x8) != 0
            || (info.flags1 & 0x1) != 0;

        // String table.
        if strtab_va != 0 {
            info.strtab_off = elf
                .vaddr_to_offset(strtab_va)
                .ok_or_else(|| ElfError("DT_STRTAB not in any PT_LOAD".into()))?;
            let end = info
                .strtab_off
                .checked_add(strsz as usize)
                .ok_or_else(|| ElfError("strtab overflow".into()))?;
            info.strtab = elf
                .data
                .get(info.strtab_off..end)
                .ok_or_else(|| ElfError("strtab exceeds file".into()))?
                .to_vec();
        }

        // Now resolve name-carrying tags.
        for d in &dyns {
            match d.tag {
                DT_NEEDED => {
                    if let Some(off) = elf.vaddr_to_offset(strtab_va + d.val) {
                        info.needed.push(elf.read_cstr(off)?);
                    }
                }
                DT_SONAME => {
                    info.soname = elf
                        .vaddr_to_offset(strtab_va + d.val)
                        .map(|o| elf.read_cstr(o))
                        .transpose()?;
                }
                DT_RPATH => {
                    info.rpath = elf
                        .vaddr_to_offset(strtab_va + d.val)
                        .map(|o| elf.read_cstr(o))
                        .transpose()?;
                }
                DT_RUNPATH => {
                    info.runpath = elf
                        .vaddr_to_offset(strtab_va + d.val)
                        .map(|o| elf.read_cstr(o))
                        .transpose()?;
                }
                _ => {}
            }
        }

        if symtab_va != 0 {
            info.symtab_off = elf
                .vaddr_to_offset(symtab_va)
                .ok_or_else(|| ElfError("DT_SYMTAB not in any PT_LOAD".into()))?;
        }

        // Hash tables.
        if gnu_hash_va != 0 {
            if let Some(off) = elf.vaddr_to_offset(gnu_hash_va) {
                info.gnu_hash = Some(parse_gnu_hash(elf.data, off)?);
            }
        }
        if hash_va != 0 {
            if let Some(off) = elf.vaddr_to_offset(hash_va) {
                info.sysv_hash = Some(parse_sysv_hash(elf.data, off)?);
            }
        }

        let count = info
            .gnu_hash
            .as_ref()
            .map(|h| h.symbol_count())
            .or_else(|| info.sysv_hash.as_ref().map(|h| h.chains.len()))
            .unwrap_or(0);
        let max_by_len = elf.data.len() / 24 + 1;
        let count = count.min(max_by_len);
        if count > 0 && info.symtab_off != 0 {
            info.symbols = elf.read_symbols(info.symtab_off, count)?;
        }

        // Symbol version array.
        if versym_va != 0 {
            if let Some(off) = elf.vaddr_to_offset(versym_va) {
                let n = info.symbols.len();
                for i in 0..n {
                    match elf.data.get(off + i * 2..off + i * 2 + 2) {
                        Some(b) => info.versym.push(u16::from_le_bytes([b[0], b[1]])),
                        None => break,
                    }
                }
            }
        }
        if verneed_va != 0 && verneed_num > 0 {
            if let Some(off) = elf.vaddr_to_offset(verneed_va) {
                info.version_names = parse_verneed(elf, off, verneed_num, strtab_va)?;
            }
        }

        for p in &elf.phdrs {
            if p.p_type == PT_TLS {
                info.tls = Some(TlsSegment {
                    vaddr: p.p_vaddr,
                    filesz: p.p_filesz,
                    memsz: p.p_memsz,
                    align: p.p_align,
                });
            }
        }

        Ok(info)
    }

    /// Name of dynamic symbol `idx`.
    pub fn symbol_name(&self, idx: usize) -> Option<&str> {
        let s = self.symbols.get(idx)?;
        let start = s.name as usize;
        let rest = self.strtab.get(start..)?;
        let end = rest.iter().position(|&b| b == 0)?;
        std::str::from_utf8(&rest[..end]).ok()
    }

    /// Version string associated with symbol `idx`, if any.
    pub fn symbol_version(&self, idx: usize) -> Option<&str> {
        let vi = *self.versym.get(idx)?;
        // 0 = local, 1 = global (unversioned).
        if vi <= 1 {
            return None;
        }
        self.version_names
            .iter()
            .find(|(idx, _)| *idx == vi)
            .map(|(_, n)| n.as_str())
    }

    /// Look up a symbol index by name using the GNU hash first, then SysV.
    pub fn lookup(&self, name: &str) -> Option<u32> {
        let bytes = name.as_bytes();
        if let Some(h) = &self.gnu_hash {
            if let Some(i) = h.lookup(bytes) {
                if self.symbol_name(i as usize) == Some(name) {
                    return Some(i);
                }
            }
        }
        if let Some(h) = &self.sysv_hash {
            if let Some(i) = h.lookup(bytes) {
                if self.symbol_name(i as usize) == Some(name) {
                    return Some(i);
                }
            }
        }
        // Linear fallback for tables without a hash section.
        self.symbols
            .iter()
            .enumerate()
            .find(|(i, _)| self.symbol_name(*i) == Some(name))
            .map(|(i, _)| i as u32)
    }
}

fn parse_gnu_hash(data: &[u8], off: usize) -> DynResult<GnuHash> {
    let g = |o: usize| -> DynResult<[u8; 4]> {
        data.get(o..o + 4)
            .ok_or_else(|| ElfError("gnu hash header oob".into()))?
            .try_into()
            .map_err(|_| ElfError("gnu hash slice".into()))
    };
    let nbuckets = u32::from_le_bytes(g(off)?);
    let symoffset = u32::from_le_bytes(g(off + 4)?);
    let bloom_size = u32::from_le_bytes(g(off + 8)?);
    let bloom_shift = u32::from_le_bytes(g(off + 12)?);
    let max_elems = data.len() / 4 + 1;
    if nbuckets as usize > max_elems || bloom_size as usize > max_elems {
        return derr("gnu hash table size implausible");
    }
    let mut p = off + 16;
    let mut bloom = Vec::with_capacity(bloom_size as usize);
    for _ in 0..bloom_size {
        let v = data
            .get(p..p + 8)
            .ok_or_else(|| ElfError("gnu hash bloom oob".into()))?;
        bloom.push(u64::from_le_bytes(v.try_into().unwrap()));
        p += 8;
    }
    let mut buckets = Vec::with_capacity(nbuckets as usize);
    for _ in 0..nbuckets {
        let v = data
            .get(p..p + 4)
            .ok_or_else(|| ElfError("gnu hash buckets oob".into()))?;
        buckets.push(u32::from_le_bytes(v.try_into().unwrap()));
        p += 4;
    }
    let chain_cap = data.len().saturating_sub(p) / 4;
    let mut chain = Vec::with_capacity(chain_cap.min(1 << 20));
    let mut q = p;
    while q + 4 <= data.len() && chain.len() < chain_cap {
        chain.push(u32::from_le_bytes(data[q..q + 4].try_into().unwrap()));
        q += 4;
    }
    Ok(GnuHash {
        nbuckets,
        symoffset,
        bloom_shift,
        bloom,
        buckets,
        chain,
    })
}

fn parse_sysv_hash(data: &[u8], off: usize) -> DynResult<SysvHash> {
    let nbuckets = u32::from_le_bytes(
        data.get(off..off + 4)
            .ok_or_else(|| ElfError("hash nbuckets oob".into()))?
            .try_into()
            .unwrap(),
    );
    let nchains = u32::from_le_bytes(
        data.get(off + 4..off + 8)
            .ok_or_else(|| ElfError("hash nchains oob".into()))?
            .try_into()
            .unwrap(),
    );
    let max_elems = data.len() / 4 + 1;
    if nbuckets as usize > max_elems || nchains as usize > max_elems {
        return derr("sysv hash table size implausible");
    }
    let mut p = off + 8;
    let mut buckets = Vec::with_capacity(nbuckets as usize);
    for _ in 0..nbuckets {
        buckets.push(u32::from_le_bytes(
            data.get(p..p + 4)
                .ok_or_else(|| ElfError("hash buckets oob".into()))?
                .try_into()
                .unwrap(),
        ));
        p += 4;
    }
    let mut chains = Vec::with_capacity(nchains as usize);
    for _ in 0..nchains {
        chains.push(u32::from_le_bytes(
            data.get(p..p + 4)
                .ok_or_else(|| ElfError("hash chains oob".into()))?
                .try_into()
                .unwrap(),
        ));
        p += 4;
    }
    Ok(SysvHash { buckets, chains })
}

fn parse_verneed(
    elf: &Elf<'_>,
    off: usize,
    num: u64,
    strtab_va: u64,
) -> DynResult<Vec<(u16, String)>> {
    let mut out = Vec::new();
    let mut cur = off;
    for _ in 0..num.min(4096) {
        let vn_cnt = u16::from_le_bytes(
            elf.data
                .get(cur + 2..cur + 4)
                .ok_or_else(|| ElfError("verneed oob".into()))?
                .try_into()
                .unwrap(),
        );
        let vn_aux = u32::from_le_bytes(
            elf.data
                .get(cur + 8..cur + 12)
                .ok_or_else(|| ElfError("verneed aux oob".into()))?
                .try_into()
                .unwrap(),
        );
        let vn_next = u32::from_le_bytes(
            elf.data
                .get(cur + 12..cur + 16)
                .ok_or_else(|| ElfError("verneed next oob".into()))?
                .try_into()
                .unwrap(),
        );
        let mut aux = cur + vn_aux as usize;
        for _ in 0..vn_cnt.min(4096) {
            let vna_other = u16::from_le_bytes(
                elf.data
                    .get(aux + 6..aux + 8)
                    .ok_or_else(|| ElfError("vernaux oob".into()))?
                    .try_into()
                    .unwrap(),
            );
            let vna_name = u32::from_le_bytes(
                elf.data
                    .get(aux + 8..aux + 12)
                    .ok_or_else(|| ElfError("vernaux name oob".into()))?
                    .try_into()
                    .unwrap(),
            );
            let vna_next = u32::from_le_bytes(
                elf.data
                    .get(aux + 12..aux + 16)
                    .ok_or_else(|| ElfError("vernaux next oob".into()))?
                    .try_into()
                    .unwrap(),
            );
            if let Some(soff) = elf.vaddr_to_offset(strtab_va + vna_name as u64) {
                if let Ok(name) = elf.read_cstr(soff) {
                    out.push((vna_other, name));
                }
            }
            if vna_next == 0 {
                break;
            }
            aux += vna_next as usize;
        }
        if vn_next == 0 {
            break;
        }
        cur += vn_next as usize;
    }
    Ok(out)
}
