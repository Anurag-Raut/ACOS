//! Native-library cross-check: which libraries does the app *ask for* that the
//! APK does not ship for the ABI it will run on?
//!
//! The motivation is the reported false green (FB2 feedback item 5): an APK whose
//! `lib/x86_64/` was missing `libworklets.so` launched, showed an empty React
//! Native surface and never mounted, because SoLoader looped on a library that
//! was not there. `acos launch` now reports that as `blocked` /
//! `PLACEHOLDER_CONTENT`, and this module is the *static* half: `acos inspect`
//! reads the references out of the APK itself and says which of them are absent
//! before anything is launched.
//!
//! Two kinds of reference are collected, both from bytes inside the APK:
//!
//! 1. **ELF `DT_NEEDED`** of every library the APK ships for the ABI. Android's
//!    linker resolves a bundled library's `NEEDED` entries from the app's lib
//!    directory first and the platform second, so a `NEEDED` name that is
//!    neither a platform library nor present under `lib/<abi>/` is a library the
//!    APK itself is supposed to provide and does not.
//! 2. **`const-string` → `invoke-static …->loadLibrary`** call sites in the DEX
//!    string tables: literally the names the app hands to
//!    `System.loadLibrary`/`SoLoader.loadLibrary`. Only a string that is the
//!    argument of such a call counts, never a bare string table hit, so a
//!    resource name or a log message that happens to look like a library cannot
//!    raise a warning.
//!
//! Platform libraries (`libc.so`, `libm.so`, …: [`SYSTEM_LIBS`]) are never
//! warnings; they come from the system image, not from the APK. A reference that
//! resolves (the file is under `lib/<abi>/`) is never a warning. The result is
//! therefore a *closed* statement: "this file is referenced and absent", never a
//! guess about why the app misbehaves.

use crate::apk::Apk;

/// `error.code`-style tag carried with every warning.
pub const NATIVE_LIB_MISSING: &str = "NATIVE_LIB_MISSING";

/// Libraries provided by the Android platform image, never by the APK.
///
/// This is the bionic / platform-served set an app links against without
/// bundling; a `NEEDED` entry for one of these is satisfied by `/system/lib64`
/// and is not a packaging mistake. Anything outside this list that a bundled
/// library needs (or that a `loadLibrary` call site names) must come from the
/// APK.
pub const SYSTEM_LIBS: &[&str] = &[
    "libEGL.so",
    "libGLESv1_CM.so",
    "libGLESv2.so",
    "libGLESv3.so",
    "libOpenMAXAL.so",
    "libOpenSLES.so",
    "libRS.so",
    "libandroid.so",
    "libandroid_runtime.so",
    "libbinder_ndk.so",
    "libc.so",
    "libcamera2ndk.so",
    "libdl.so",
    "libjnigraphics.so",
    "liblog.so",
    "libm.so",
    "libmediandk.so",
    "libnativehelper.so",
    "libnativewindow.so",
    "libneuralnetworks.so",
    "libstdc++.so",
    "libsync.so",
    "libvulkan.so",
    "libz.so",
];

/// One library the app references that `lib/<abi>/` does not contain.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NativeLibWarning {
    /// Always [`NATIVE_LIB_MISSING`]; the field exists so an agent can branch on
    /// one stable token instead of parsing a message.
    pub code: &'static str,
    /// The library file name, including the `lib` prefix and `.so` suffix.
    pub lib: String,
    /// The ABI directory the check ran against (`lib/<abi>/`).
    pub abi: String,
    /// How the name was found: `elf-needed` (DT_NEEDED of a bundled library) or
    /// `dex-load-library` (a `loadLibrary` call argument).
    pub source: &'static str,
    /// For `elf-needed`: the bundled libraries whose `NEEDED` entry names it.
    /// For `dex-load-library`: the DEX entry names carrying the call site.
    pub referenced_by: Vec<String>,
}

/// The ABI an inspect check should run against: the host ABI when the APK has
/// it (the ABI ACOS will execute), else the APK's first (sorted) ABI.
pub fn checked_abi(apk: &Apk, host_abi: &str) -> Option<String> {
    let abis = apk.native_abis();
    if abis.iter().any(|abi| abi == host_abi) {
        return Some(host_abi.to_string());
    }
    abis.into_iter().next()
}

/// Every referenced-but-missing native library for `abi`, sorted by name.
pub fn missing_native_libs(apk: &Apk, abi: &str) -> Vec<NativeLibWarning> {
    let present: Vec<String> = apk
        .native_libs
        .get(abi)
        .map(|libs| libs.iter().map(|l| l.name.clone()).collect())
        .unwrap_or_default();
    let mut warnings: Vec<NativeLibWarning> = Vec::new();

    // 1. DT_NEEDED of the libraries this APK ships for the ABI.
    for name in &present {
        let Some(bytes) = apk.entry_bytes(&format!("lib/{abi}/{name}")) else {
            continue;
        };
        for needed in elf_needed(&bytes) {
            record(
                &mut warnings,
                &present,
                &needed,
                abi,
                "elf-needed",
                name,
            );
        }
    }

    // 2. The names the app's own `loadLibrary` call sites ask for.
    for dex in &apk.dex_files {
        for name in dex_load_library_names(&dex.bytes) {
            record(
                &mut warnings,
                &present,
                &name,
                abi,
                "dex-load-library",
                &dex.name,
            );
        }
    }

    warnings.sort_by(|a, b| a.lib.cmp(&b.lib).then(a.source.cmp(b.source)));
    warnings
}

fn record(
    warnings: &mut Vec<NativeLibWarning>,
    present: &[String],
    candidate: &str,
    abi: &str,
    source: &'static str,
    by: &str,
) {
    if !is_library(candidate) || SYSTEM_LIBS.contains(&candidate) {
        return;
    }
    if present.iter().any(|name| name == candidate) {
        return;
    }
    if let Some(existing) = warnings
        .iter_mut()
        .find(|w| w.lib == candidate && w.source == source)
    {
        if !existing.referenced_by.iter().any(|n| n == by) {
            existing.referenced_by.push(by.to_string());
        }
        return;
    }
    warnings.push(NativeLibWarning {
        code: NATIVE_LIB_MISSING,
        lib: candidate.to_string(),
        abi: abi.to_string(),
        source,
        referenced_by: vec![by.to_string()],
    });
}

/// A plain `lib<name>.so` file name — no directory, no `..`, bounded length.
fn is_library(name: &str) -> bool {
    name.len() > 6
        && name.len() <= 128
        && name.starts_with("lib")
        && name.ends_with(".so")
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && name[3..name.len() - 3]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-' | '.'))
}

// ---------------------------------------------------------------- ELF ------

/// `DT_NEEDED` names of an ELF32/ELF64 little-endian object, in file order.
///
/// A malformed object yields whatever was parsed before the defect: this is a
/// *warning* channel, so a truncated library must not turn an inspect call into
/// an error. Every read is bounds-checked.
pub fn elf_needed(data: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    if data.len() < 64 || &data[0..4] != b"\x7fELF" {
        return out;
    }
    let is64 = match data[4] {
        2 => true,
        1 => false,
        _ => return out,
    };
    if data[5] != 1 {
        // Big-endian: Android's ABIs are little-endian; nothing to parse.
        return out;
    }
    let u16_at = |off: usize| -> Option<u16> {
        data.get(off..off + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |off: usize| -> Option<u32> {
        data.get(off..off + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let u64_at = |off: usize| -> Option<u64> {
        data.get(off..off + 8).map(|b| {
            u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
        })
    };

    let (shoff, shentsize, shnum, _shstrndx) = if is64 {
        (
            u64_at(0x28).unwrap_or(0) as usize,
            u16_at(0x3a).unwrap_or(0) as usize,
            u16_at(0x3c).unwrap_or(0) as usize,
            u16_at(0x3e).unwrap_or(0) as usize,
        )
    } else {
        (
            u32_at(0x20).unwrap_or(0) as usize,
            u16_at(0x2e).unwrap_or(0) as usize,
            u16_at(0x30).unwrap_or(0) as usize,
            u16_at(0x32).unwrap_or(0) as usize,
        )
    };
    if shoff == 0 || shentsize == 0 || shnum == 0 {
        return out;
    }

    // (type, offset, size, link)
    let mut sections: Vec<(u32, usize, usize, usize)> = Vec::with_capacity(shnum);
    for i in 0..shnum {
        let base = shoff + i * shentsize;
        let section = if is64 {
            let (typ, off, size, link) = match (
                u32_at(base + 4),
                u64_at(base + 0x18),
                u64_at(base + 0x20),
                u32_at(base + 0x28),
            ) {
                (Some(t), Some(o), Some(s), Some(l)) => (t, o as usize, s as usize, l as usize),
                _ => return out,
            };
            (typ, off, size, link)
        } else {
            let (typ, off, size, link) = match (
                u32_at(base + 4),
                u32_at(base + 0x10),
                u32_at(base + 0x14),
                u32_at(base + 0x18),
            ) {
                (Some(t), Some(o), Some(s), Some(l)) => (t, o as usize, s as usize, l as usize),
                _ => return out,
            };
            (typ, off, size, link)
        };
        sections.push(section);
    }

    for (typ, off, size, link) in sections.iter().copied() {
        if typ != 6 {
            // SHT_DYNAMIC
            continue;
        }
        let Some(strtab) = sections.get(link) else {
            continue;
        };
        let (str_off, str_size) = (strtab.1, strtab.2);
        let entry_size = if is64 { 16 } else { 8 };
        let mut pos = off;
        while pos + entry_size <= off + size && pos + entry_size <= data.len() {
            let (tag, val) = if is64 {
                match (u64_at(pos), u64_at(pos + 8)) {
                    (Some(t), Some(v)) => (t, v as usize),
                    _ => break,
                }
            } else {
                match (u32_at(pos), u32_at(pos + 4)) {
                    (Some(t), Some(v)) => (t as u64, v as usize),
                    _ => break,
                }
            };
            if tag == 0 {
                break;
            }
            if tag == 1 {
                let start = str_off.saturating_add(val);
                if start < data.len() && start < str_off.saturating_add(str_size) {
                    let rest = &data[start..data.len().min(str_off + str_size)];
                    if let Some(end) = rest.iter().position(|b| *b == 0) {
                        if let Ok(name) = std::str::from_utf8(&rest[..end]) {
                            out.push(name.to_string());
                        }
                    }
                }
            }
            pos += entry_size;
        }
    }
    out
}

// ---------------------------------------------------------------- DEX ------

/// Strings handed to a `loadLibrary(String…)` call in this DEX.
///
/// A name counts only when a `const-string`/`const-string/jumbo` naming it is
/// followed within a few instructions by an `invoke-static`/`invoke-static/range`
/// whose method is named `loadLibrary` (`System.loadLibrary`,
/// `SoLoader.loadLibrary`, and the JNI helper wrappers share that name). The
/// check is deliberately narrow: it exists to be trustworthy on a stock APK, so
/// a bare string that merely looks like a library is never reported.
pub fn dex_load_library_names(data: &[u8]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if data.len() < 0x70 || &data[0..4] != b"dex\n" {
        return out;
    }
    let u16_at = |off: usize| -> Option<u16> {
        data.get(off..off + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |off: usize| -> Option<u32> {
        data.get(off..off + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let (Some(string_ids_size), Some(string_ids_off)) = (u32_at(56), u32_at(60)) else {
        return out;
    };
    let (Some(type_ids_size), Some(type_ids_off)) = (u32_at(64), u32_at(68)) else {
        return out;
    };
    let (Some(method_ids_size), Some(method_ids_off)) = (u32_at(88), u32_at(92)) else {
        return out;
    };

    // Bounded, overflow-safe section checks: a hostile header must not make this
    // read outside the buffer.
    let section_ok = |off: u32, count: u32, item: u64| -> bool {
        let start = off as u64;
        let len = count as u64 * item;
        start.saturating_add(len) <= data.len() as u64
    };
    if !section_ok(string_ids_off, string_ids_size, 4)
        || !section_ok(type_ids_off, type_ids_size, 4)
        || !section_ok(method_ids_off, method_ids_size, 8)
    {
        return out;
    }

    // Only strings short enough to be a library name are ever decoded, and only
    // when a call site actually names them: decoding the whole table of a
    // 37 000-string DEX would cost far more than this check is worth.
    let mut load_library_methods: Vec<u32> = Vec::new();
    for i in 0..method_ids_size as usize {
        let base = method_ids_off as usize + i * 8;
        let (Some(class_idx), Some(name_idx)) = (u16_at(base), u32_at(base + 4)) else {
            return out;
        };
        let Some(class_name) = dex_string(data, string_ids_off, string_ids_size, {
            let Some(type_idx) = u16_at(type_ids_off as usize + class_idx as usize * 4) else {
                return out;
            };
            let Some(descriptor_idx) = u32_at(type_ids_off as usize + type_idx as usize * 4) else {
                return out;
            };
            descriptor_idx
        }) else {
            continue;
        };
        if !class_name.contains("System") && !class_name.contains("SoLoader") {
            continue;
        }
        if let Some(name) = dex_string(data, string_ids_off, string_ids_size, name_idx) {
            if name == "loadLibrary" {
                load_library_methods.push(i as u32);
            }
        }
    }
    if load_library_methods.is_empty() {
        return out;
    }

    let mut i = 0usize;
    while i + 6 < data.len() {
        let (string_idx, next) = match data[i] {
            0x1a => match u16_at(i + 2) {
                Some(idx) => (idx as u32, i + 4),
                None => break,
            },
            0x1b => match u32_at(i + 2) {
                Some(idx) => (idx, i + 6),
                None => break,
            },
            _ => {
                i += 1;
                continue;
            }
        };
        i += 1;
        if string_idx >= string_ids_size {
            continue;
        }
        let limit = (next + 16).min(data.len().saturating_sub(6));
        let mut j = next;
        let mut found = None;
        while j <= limit {
            if matches!(data[j], 0x71 | 0x77) {
                if let Some(method_idx) = u16_at(j + 4) {
                    if load_library_methods.contains(&(method_idx as u32)) {
                        found = Some(());
                        break;
                    }
                }
            }
            j += 1;
        }
        if found.is_none() {
            continue;
        }
        let Some(raw) = dex_string(data, string_ids_off, string_ids_size, string_idx) else {
            continue;
        };
        // A `loadLibrary` argument is a bare handle: `System.loadLibrary("foo")`,
        // `SoLoader.loadLibrary("foo")`. A `loadLibrary("libfoo.so")` form is
        // accepted too and normalised to the file name.
        let handle = raw.trim();
        let file = if handle.ends_with(".so") {
            handle.to_string()
        } else {
            format!("lib{handle}.so")
        };
        if is_library(&file) && !out.contains(&file) {
            out.push(file);
        }
    }
    out
}

/// Decode one `string_data_item` of a DEX string table (MUTF-8, NUL-terminated).
fn dex_string(data: &[u8], string_ids_off: u32, string_ids_size: u32, index: u32) -> Option<String> {
    if index >= string_ids_size {
        return None;
    }
    let base = string_ids_off as usize + index as usize * 4;
    let data_off = u32::from_le_bytes(data.get(base..base + 4)?.try_into().ok()?) as usize;
    // uleb128 utf16 length
    let mut pos = data_off;
    loop {
        let byte = *data.get(pos)?;
        pos += 1;
        if byte & 0x80 == 0 {
            break;
        }
    }
    let rest = data.get(pos..)?;
    let end = rest.iter().position(|b| *b == 0)?;
    if end > 256 {
        return None;
    }
    std::str::from_utf8(&rest[..end]).ok().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_libraries_are_never_warnings() {
        for lib in ["libc.so", "libm.so", "liblog.so", "libjnigraphics.so"] {
            assert!(SYSTEM_LIBS.contains(&lib), "{lib} must stay in SYSTEM_LIBS");
            assert!(is_library(lib));
        }
    }

    #[test]
    fn a_library_name_is_a_plain_soname() {
        assert!(is_library("libworklets.so"));
        assert!(is_library("libc++_shared.so"));
        assert!(is_library("libreact_codegen_rncore.so"));
        assert!(!is_library("worklets"));
        assert!(!is_library("libworklets"));
        assert!(!is_library("lib/../libworklets.so"));
        assert!(!is_library("liba b.so"));
        assert!(!is_library("lib.so"));
    }

    #[test]
    fn elf_needed_reads_a_minimal_dynamic_section() {
        let data = tiny_elf(&["libworklets.so", "libc.so"]);
        assert_eq!(elf_needed(&data), vec!["libworklets.so", "libc.so"]);
        // Truncation and non-ELF input are empty, never a panic.
        assert!(elf_needed(&data[..0x40]).is_empty());
        assert!(elf_needed(b"not an elf at all").is_empty());
        assert!(elf_needed(&[]).is_empty());
    }

    /// A 64-bit little-endian ELF carrying exactly these `DT_NEEDED` names.
    fn tiny_elf(needed: &[&str]) -> Vec<u8> {
        let mut data = vec![0u8; 0x400];
        data[0..4].copy_from_slice(b"\x7fELF");
        data[4] = 2; // 64-bit
        data[5] = 1; // little-endian
        let mut strtab: Vec<u8> = vec![0];
        let mut offsets = Vec::new();
        for name in needed {
            offsets.push(strtab.len() as u64);
            strtab.extend_from_slice(name.as_bytes());
            strtab.push(0);
        }
        let str_off = 0x100usize;
        data[str_off..str_off + strtab.len()].copy_from_slice(&strtab);
        let dyn_off = 0x200usize;
        let mut entries: Vec<(u64, u64)> = offsets.into_iter().map(|o| (1, o)).collect();
        entries.push((0, 0));
        for (n, (tag, val)) in entries.iter().enumerate() {
            let at = dyn_off + n * 16;
            data[at..at + 8].copy_from_slice(&tag.to_le_bytes());
            data[at + 8..at + 16].copy_from_slice(&val.to_le_bytes());
        }
        // Section headers: [0] null, [1] .dynstr, [2] .dynamic
        let shoff = 0x300usize;
        data[0x28..0x30].copy_from_slice(&(shoff as u64).to_le_bytes());
        data[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes());
        data[0x3c..0x3e].copy_from_slice(&3u16.to_le_bytes());
        let put = |data: &mut Vec<u8>, idx: usize, typ: u32, off: u64, size: u64, link: u32| {
            let base = shoff + idx * 64;
            data[base + 4..base + 8].copy_from_slice(&typ.to_le_bytes());
            data[base + 0x18..base + 0x20].copy_from_slice(&off.to_le_bytes());
            data[base + 0x20..base + 0x28].copy_from_slice(&size.to_le_bytes());
            data[base + 0x28..base + 0x2c].copy_from_slice(&link.to_le_bytes());
        };
        put(&mut data, 1, 3, str_off as u64, strtab.len() as u64, 0); // SHT_STRTAB
        put(
            &mut data,
            2,
            6,
            dyn_off as u64,
            (entries.len() * 16) as u64,
            1,
        ); // SHT_DYNAMIC
        data
    }

    /// A DEX with one `SoLoader.loadLibrary("worklets")` call site.
    fn tiny_dex_with_load_library(handle: &str) -> Vec<u8> {
        let mut data = vec![0u8; 0x200];
        data[0..8].copy_from_slice(b"dex\n035\0");
        let strings = ["Lcom/facebook/soloader/SoLoader;", "loadLibrary", handle];
        let mut payload: Vec<u8> = Vec::new();
        let mut string_offsets = Vec::new();
        for s in strings {
            string_offsets.push((0x100 + payload.len()) as u32);
            // uleb128 utf16 length (all ASCII here)
            payload.push(s.len() as u8);
            payload.extend_from_slice(s.as_bytes());
            payload.push(0);
        }
        data[0x100..0x100 + payload.len()].copy_from_slice(&payload);

        // string_ids @0x80, type_ids @0x90, method_ids @0x98, code
        for (n, off) in string_offsets.iter().enumerate() {
            data[0x80 + n * 4..0x84 + n * 4].copy_from_slice(&off.to_le_bytes());
        }
        data[0x90..0x94].copy_from_slice(&0u32.to_le_bytes()); // type 0 -> strings[0]
        data[0x98..0x9a].copy_from_slice(&0u16.to_le_bytes()); // class_idx
        data[0x9a..0x9c].copy_from_slice(&0u16.to_le_bytes()); // proto_idx
        data[0x9c..0xa0].copy_from_slice(&1u32.to_le_bytes()); // name_idx = loadLibrary

        // const-string v0, strings[2]  ;  invoke-static {v0}, method@0
        let mut code = vec![0x1a, 0x00, 0x02, 0x00];
        code.extend_from_slice(&[0x71, 0x00, 0x00, 0x00, 0x00, 0x00]);
        data[0xb0..0xb0 + code.len()].copy_from_slice(&code);

        let put32 = |data: &mut Vec<u8>, at: usize, v: u32| {
            data[at..at + 4].copy_from_slice(&v.to_le_bytes())
        };
        put32(&mut data, 56, strings.len() as u32);
        put32(&mut data, 60, 0x80);
        put32(&mut data, 64, 1);
        put32(&mut data, 68, 0x90);
        put32(&mut data, 88, 1);
        put32(&mut data, 92, 0x98);
        data
    }

    fn zip_of(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut zip, bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn dex_call_site_names_are_read_and_bare_strings_are_not() {
        let names = dex_load_library_names(&tiny_dex_with_load_library("worklets"));
        assert_eq!(names, vec!["libworklets.so"]);
        // A `libfoo.so`-style argument is normalised to the same file name.
        let names = dex_load_library_names(&tiny_dex_with_load_library("libfoo.so"));
        assert_eq!(names, vec!["libfoo.so"]);
        // Truncated / non-DEX input never panics and never guesses.
        assert!(dex_load_library_names(b"").is_empty());
        assert!(dex_load_library_names(b"dex\n035\0garbage").is_empty());
    }

    #[test]
    fn a_referenced_library_that_is_absent_is_the_only_warning() {
        let dex = tiny_dex_with_load_library("worklets");
        let elf = tiny_elf(&["libworklets.so", "libc.so"]);
        let abi = "x86_64";

        // Stock: the APK ships what it references -> no warnings at all.
        let stock = crate::Apk::from_bytes(zip_of(&[
            ("classes.dex", dex.clone()),
            ("lib/x86_64/libreanimated.so", elf.clone()),
            ("lib/x86_64/libworklets.so", Vec::new()),
        ]))
        .unwrap();
        assert!(missing_native_libs(&stock, abi).is_empty());

        // Crafted: one library removed -> exactly that library, both ways.
        let crafted = crate::Apk::from_bytes(zip_of(&[
            ("classes.dex", dex),
            ("lib/x86_64/libreanimated.so", elf),
        ]))
        .unwrap();
        let warnings = missing_native_libs(&crafted, abi);
        assert_eq!(warnings.len(), 2, "one per source: {warnings:?}");
        for warning in &warnings {
            assert_eq!(warning.code, NATIVE_LIB_MISSING);
            assert_eq!(warning.lib, "libworklets.so");
            assert_eq!(warning.abi, abi);
            assert!(!warning.referenced_by.is_empty());
        }
        let sources: Vec<&str> = warnings.iter().map(|w| w.source).collect();
        assert_eq!(sources, vec!["dex-load-library", "elf-needed"]);
        assert_eq!(checked_abi(&crafted, abi).as_deref(), Some(abi));
        // A platform library is never a warning, even though nothing ships it.
        assert!(!missing_native_libs(&crafted, abi)
            .iter()
            .any(|w| w.lib == "libc.so"));
    }
}
