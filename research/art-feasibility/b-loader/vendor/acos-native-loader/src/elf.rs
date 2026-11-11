//! Minimal, dependency-free, panic-free ELF64 x86_64 parser.
//!
//! Only what the loader needs is parsed: the ELF header, the program header
//! table, and the `PT_DYNAMIC` entries. Every read is bounds checked and
//! returns [`ElfError`] instead of panicking, which is what makes the parser
//! safe to fuzz (see `tests/fuzz.rs`).

use std::fmt;

/// ELF parsing failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfError(pub String);

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ElfError {}

fn err<T>(msg: impl Into<String>) -> Result<T, ElfError> {
    Err(ElfError(msg.into()))
}

// --- ELF constants ---------------------------------------------------------

pub const ELFMAG: [u8; 4] = [0x7f, b'E', b'L', b'F'];
pub const ELFCLASS64: u8 = 2;
pub const ELFDATA2LSB: u8 = 1;
pub const EV_CURRENT: u8 = 1;
pub const ET_EXEC: u16 = 2;
pub const ET_DYN: u16 = 3;
pub const EM_X86_64: u16 = 62;
pub const EM_AARCH64: u16 = 183;

// AArch64 dynamic relocation types (`elf.h`). Only the subset the loader
// implements is named; anything else is a structured UNSUPPORTED_RELOCATION.
pub const R_AARCH64_NONE: u32 = 0;
pub const R_AARCH64_ABS64: u32 = 257;
pub const R_AARCH64_COPY: u32 = 1024;
pub const R_AARCH64_GLOB_DAT: u32 = 1025;
pub const R_AARCH64_JUMP_SLOT: u32 = 1026;
pub const R_AARCH64_RELATIVE: u32 = 1027;
pub const R_AARCH64_TLS_DTPMOD64: u32 = 1028;
pub const R_AARCH64_TLS_DTPREL64: u32 = 1029;
pub const R_AARCH64_TLS_TPREL64: u32 = 1030;
pub const R_AARCH64_TLSDESC: u32 = 1031;
pub const R_AARCH64_IRELATIVE: u32 = 1032;

/// The ELF machine this *host runtime* was built for.
///
/// The loader maps guest objects into the caller's address space and calls
/// them with the caller's ABI, so the guest object must match the host
/// instruction set. On the x86_64 product host this is `x86_64`; the W194-A
/// aarch64 cross-build accepts the arm64-v8a objects it is meant to load.
#[cfg(target_arch = "x86_64")]
pub const HOST_ELF_MACHINE: u16 = EM_X86_64;
#[cfg(target_arch = "aarch64")]
pub const HOST_ELF_MACHINE: u16 = EM_AARCH64;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("acos-native-loader: unsupported host architecture");

/// Human-readable name of [`HOST_ELF_MACHINE`] for structured errors.
#[cfg(target_arch = "x86_64")]
pub const HOST_ABI_NAME: &str = "x86_64";
#[cfg(target_arch = "aarch64")]
pub const HOST_ABI_NAME: &str = "aarch64";

pub const PT_NULL: u32 = 0;
pub const PT_LOAD: u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_INTERP: u32 = 3;
pub const PT_NOTE: u32 = 4;
pub const PT_PHDR: u32 = 6;
pub const PT_TLS: u32 = 7;
pub const PT_GNU_EH_FRAME: u32 = 0x6474_e550;
pub const PT_GNU_STACK: u32 = 0x6474_e551;
pub const PT_GNU_RELRO: u32 = 0x6474_e552;

pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

// Dynamic tags.
pub const DT_NULL: i64 = 0;
pub const DT_NEEDED: i64 = 1;
pub const DT_PLTRELSZ: i64 = 2;
pub const DT_PLTGOT: i64 = 3;
pub const DT_HASH: i64 = 4;
pub const DT_STRTAB: i64 = 5;
pub const DT_SYMTAB: i64 = 6;
pub const DT_RELA: i64 = 7;
pub const DT_RELASZ: i64 = 8;
pub const DT_RELAENT: i64 = 9;
pub const DT_STRSZ: i64 = 10;
pub const DT_SYMENT: i64 = 11;
pub const DT_INIT: i64 = 12;
pub const DT_FINI: i64 = 13;
pub const DT_SONAME: i64 = 14;
pub const DT_RPATH: i64 = 15;
pub const DT_SYMBOLIC: i64 = 16;
pub const DT_REL: i64 = 17;
pub const DT_RELSZ: i64 = 18;
pub const DT_RELENT: i64 = 19;
pub const DT_PLTREL: i64 = 20;
pub const DT_DEBUG: i64 = 21;
pub const DT_TEXTREL: i64 = 22;
pub const DT_JMPREL: i64 = 23;
pub const DT_BIND_NOW: i64 = 24;
pub const DT_INIT_ARRAY: i64 = 25;
pub const DT_FINI_ARRAY: i64 = 26;
pub const DT_INIT_ARRAYSZ: i64 = 27;
pub const DT_FINI_ARRAYSZ: i64 = 28;
pub const DT_RUNPATH: i64 = 29;
pub const DT_FLAGS: i64 = 30;
pub const DT_PREINIT_ARRAY: i64 = 32;
pub const DT_PREINIT_ARRAYSZ: i64 = 33;
pub const DT_SYMTAB_SHNDX: i64 = 34;
pub const DT_RELRSZ: i64 = 35;
pub const DT_RELR: i64 = 36;
pub const DT_RELRENT: i64 = 37;
pub const DT_FLAGS_1: i64 = 0x6fff_fffb;
pub const DT_VERSYM: i64 = 0x6fff_fff0;
pub const DT_VERDEF: i64 = 0x6fff_fffc;
pub const DT_VERDEFNUM: i64 = 0x6fff_fffd;
pub const DT_VERNEED: i64 = 0x6fff_fffe;
pub const DT_VERNEEDNUM: i64 = 0x6fff_ffff;
pub const DT_GNU_HASH: i64 = 0x6fff_fef5;
pub const DT_RELACOUNT: i64 = 0x6fff_fff9;
pub const DT_RELCOUNT: i64 = 0x6fff_fffa;

// Android packed relocation tags.
pub const DT_ANDROID_REL: i64 = 0x6000_000f;
pub const DT_ANDROID_RELSZ: i64 = 0x6000_0010;
pub const DT_ANDROID_RELA: i64 = 0x6000_0011;
pub const DT_ANDROID_RELASZ: i64 = 0x6000_0012;
pub const DT_ANDROID_RELR: i64 = 0x6fff_e000;
pub const DT_ANDROID_RELRSZ: i64 = 0x6fff_e001;
pub const DT_ANDROID_RELRENT: i64 = 0x6fff_e003;

// Symbol table.
pub const STB_LOCAL: u8 = 0;
pub const STB_GLOBAL: u8 = 1;
pub const STB_WEAK: u8 = 2;
pub const STT_NOTYPE: u8 = 0;
pub const STT_OBJECT: u8 = 1;
pub const STT_FUNC: u8 = 2;
pub const STT_SECTION: u8 = 3;
pub const STT_FILE: u8 = 4;
pub const STT_COMMON: u8 = 5;
pub const STT_TLS: u8 = 6;
pub const SHN_UNDEF: u16 = 0;
pub const SHN_ABS: u16 = 0xfff1;
pub const SHN_COMMON: u16 = 0xfff2;

/// Relocation types (x86_64).
pub const R_X86_64_NONE: u32 = 0;
pub const R_X86_64_64: u32 = 1;
pub const R_X86_64_PC32: u32 = 2;
pub const R_X86_64_GLOB_DAT: u32 = 6;
pub const R_X86_64_JUMP_SLOT: u32 = 7;
pub const R_X86_64_RELATIVE: u32 = 8;
pub const R_X86_64_32: u32 = 10;
pub const R_X86_64_32S: u32 = 11;
pub const R_X86_64_COPY: u32 = 5;
pub const R_X86_64_TLS_DTPMOD: u32 = 16;
pub const R_X86_64_TLS_DTPOFF: u32 = 17;
pub const R_X86_64_TLS_TPOFF: u32 = 18;
// B-LOADER PATCH: TLS access models emitted by the Android platform build.
pub const R_X86_64_TLSGD: u32 = 19;
pub const R_X86_64_TLSLD: u32 = 20;
pub const R_X86_64_GOTTPOFF: u32 = 22;
pub const R_X86_64_TPOFF32: u32 = 23;
pub const R_X86_64_IRELATIVE: u32 = 37;
pub const R_X86_64_TLSDESC: u32 = 36;

// --- primitive readers -----------------------------------------------------

fn rd_u16(data: &[u8], off: usize) -> Result<u16, ElfError> {
    let b = data
        .get(off..off + 2)
        .ok_or_else(|| ElfError(format!("read u16 out of bounds at {off}")))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn rd_u32(data: &[u8], off: usize) -> Result<u32, ElfError> {
    let b = data
        .get(off..off + 4)
        .ok_or_else(|| ElfError(format!("read u32 out of bounds at {off}")))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn rd_u64(data: &[u8], off: usize) -> Result<u64, ElfError> {
    let b = data
        .get(off..off + 8)
        .ok_or_else(|| ElfError(format!("read u64 out of bounds at {off}")))?;
    let mut a = [0u8; 8];
    a.copy_from_slice(b);
    Ok(u64::from_le_bytes(a))
}

fn rd_i64(data: &[u8], off: usize) -> Result<i64, ElfError> {
    Ok(rd_u64(data, off)? as i64)
}

// --- structures ------------------------------------------------------------

/// The ELF file header (64-bit variant only).
#[derive(Debug, Clone, Copy)]
pub struct ElfHeader {
    pub e_type: u16,
    pub e_machine: u16,
    pub e_version: u32,
    pub e_entry: u64,
    pub e_phoff: u64,
    pub e_shoff: u64,
    pub e_flags: u32,
    pub e_ehsize: u16,
    pub e_phentsize: u16,
    pub e_phnum: u16,
    pub e_shentsize: u16,
    pub e_shnum: u16,
    pub e_shstrndx: u16,
}

/// A program header.
///
/// `#[repr(C)]` is required: the in-memory header array is handed to guest
/// unwinders through `dl_iterate_phdr`, which reads it as `Elf64_Phdr`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_paddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}

/// A `PT_DYNAMIC` entry.
#[derive(Debug, Clone, Copy)]
pub struct Dyn {
    pub tag: i64,
    pub val: u64,
}

/// A dynamic symbol (subset of fields the loader uses).
#[derive(Debug, Clone, Copy)]
pub struct Sym {
    pub name: u32,
    pub info: u8,
    pub other: u8,
    pub shndx: u16,
    pub value: u64,
    pub size: u64,
}

impl Sym {
    pub fn bind(&self) -> u8 {
        self.info >> 4
    }
    pub fn sym_type(&self) -> u8 {
        self.info & 0xf
    }
    pub fn is_undefined(&self) -> bool {
        self.shndx == SHN_UNDEF
    }
}

/// Parsed ELF image; borrows the input bytes.
pub struct Elf<'a> {
    pub data: &'a [u8],
    pub header: ElfHeader,
    pub phdrs: Vec<Phdr>,
}

impl<'a> Elf<'a> {
    /// Parse the ELF header and program header table.
    ///
    /// # Errors
    /// Returns [`ElfError`] for anything malformed. Never panics.
    pub fn parse(data: &'a [u8]) -> Result<Self, ElfError> {
        if data.len() < 64 {
            return err("file shorter than ELF64 header");
        }
        if data.get(0..4) != Some(&ELFMAG) {
            return err("bad ELF magic");
        }
        if data[4] != ELFCLASS64 {
            return err(format!(
                "not ELF64 (EI_CLASS={}); 32-bit ELF is unsupported",
                data[4]
            ));
        }
        if data[5] != ELFDATA2LSB {
            return err(format!("not little-endian (EI_DATA={})", data[5]));
        }

        let e_type = rd_u16(data, 16)?;
        let e_machine = rd_u16(data, 18)?;
        let e_version = rd_u32(data, 20)?;
        let header = ElfHeader {
            e_type,
            e_machine,
            e_version,
            e_entry: rd_u64(data, 24)?,
            e_phoff: rd_u64(data, 32)?,
            e_shoff: rd_u64(data, 40)?,
            e_flags: rd_u32(data, 48)?,
            e_ehsize: rd_u16(data, 52)?,
            e_phentsize: rd_u16(data, 54)?,
            e_phnum: rd_u16(data, 56)?,
            e_shentsize: rd_u16(data, 58)?,
            e_shnum: rd_u16(data, 60)?,
            e_shstrndx: rd_u16(data, 62)?,
        };

        if header.e_phentsize != 56 {
            return err(format!(
                "unexpected e_phentsize {} (expected 56)",
                header.e_phentsize
            ));
        }
        let mut phdrs = Vec::with_capacity(header.e_phnum as usize);
        for i in 0..header.e_phnum as usize {
            let off = header
                .e_phoff
                .checked_add((i as u64) * 56)
                .and_then(|v| usize::try_from(v).ok())
                .ok_or_else(|| ElfError("program header offset overflow".into()))?;
            phdrs.push(Phdr {
                p_type: rd_u32(data, off)?,
                p_flags: rd_u32(data, off + 4)?,
                p_offset: rd_u64(data, off + 8)?,
                p_vaddr: rd_u64(data, off + 16)?,
                p_paddr: rd_u64(data, off + 24)?,
                p_filesz: rd_u64(data, off + 32)?,
                p_memsz: rd_u64(data, off + 40)?,
                p_align: rd_u64(data, off + 48)?,
            });
        }
        Ok(Elf {
            data,
            header,
            phdrs,
        })
    }

    /// Parse the `PT_DYNAMIC` entry list. Returns an empty list when absent.
    pub fn dynamic_entries(&self) -> Result<Vec<Dyn>, ElfError> {
        let mut out = Vec::new();
        for p in &self.phdrs {
            if p.p_type != PT_DYNAMIC {
                continue;
            }
            let mut off = usize::try_from(p.p_offset)
                .map_err(|_| ElfError("dynamic offset overflow".into()))?;
            let end = off
                .checked_add(usize::try_from(p.p_filesz).unwrap_or(0))
                .ok_or_else(|| ElfError("dynamic size overflow".into()))?;
            if end > self.data.len() {
                return err("dynamic segment exceeds file");
            }
            while off + 16 <= end {
                let tag = rd_i64(self.data, off)?;
                let val = rd_u64(self.data, off + 8)?;
                out.push(Dyn { tag, val });
                off += 16;
                if tag == DT_NULL {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Read `count` dynamic symbols starting at `symtab_off`.
    pub fn read_symbols(
        &self,
        symtab_off: usize,
        count: usize,
    ) -> Result<Vec<Sym>, ElfError> {
        let mut out = Vec::with_capacity(count);
        let mut off = symtab_off;
        for _ in 0..count {
            if off + 24 > self.data.len() {
                return err("dynamic symbol table exceeds file");
            }
            let s = Sym {
                name: rd_u32(self.data, off)?,
                info: *self
                    .data
                    .get(off + 4)
                    .ok_or_else(|| ElfError("sym info oob".into()))?,
                other: *self
                    .data
                    .get(off + 5)
                    .ok_or_else(|| ElfError("sym other oob".into()))?,
                shndx: rd_u16(self.data, off + 6)?,
                value: rd_u64(self.data, off + 8)?,
                size: rd_u64(self.data, off + 16)?,
            };
            out.push(s);
            off += 24;
        }
        Ok(out)
    }

    /// Read a NUL-terminated string from the given file offset.
    pub fn read_cstr(&self, off: usize) -> Result<String, ElfError> {
        if off >= self.data.len() {
            return err(format!("string offset {off} out of bounds"));
        }
        let tail = &self.data[off..];
        let end = tail
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| ElfError("unterminated string".into()))?;
        Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
    }
}

impl<'a> Elf<'a> {
    /// Translate a virtual address to a file offset using the `PT_LOAD` map.
    pub fn vaddr_to_offset(&self, vaddr: u64) -> Option<usize> {
        for p in &self.phdrs {
            if p.p_type != PT_LOAD || p.p_memsz == 0 {
                continue;
            }
            if vaddr >= p.p_vaddr && vaddr < p.p_vaddr.saturating_add(p.p_filesz) {
                let delta = vaddr - p.p_vaddr;
                return usize::try_from(p.p_offset + delta).ok();
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_short_input() {
        assert!(Elf::parse(&[]).is_err());
        assert!(Elf::parse(&[0x7f, b'E', b'L', b'F']).is_err());
    }

    #[test]
    fn rejects_bad_magic() {
        let data = vec![0u8; 128];
        assert!(Elf::parse(&data).is_err());
    }
}

