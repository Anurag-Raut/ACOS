//! W17: the per-run native-library install (`native_libs::install`).
//!
//! These are the hostile-zip cases with teeth: a traversal-shaped `lib/` entry
//! never becomes a catalogue entry, nothing is written outside the destination,
//! an arm64-only APK is a structured `UnsupportedAbi` (never a guessed install),
//! and an APK without native code installs nothing.
//!
//! The synthetic APKs are built here with a minimal stored-only zip writer so
//! the test does not need an external tool; `acos-apk` reads it like any APK.

use std::io::Write;
use std::path::{Path, PathBuf};

use acos_services::native_libs::{self, NativeLibInstall};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cache/tests")
        .join(format!("w17-native-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

// ---------------------------------------------------------------- zip writer

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// A stored-only zip with the given `(name, bytes)` entries.
fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let name_len = name.len() as u16;
        let size = data.len() as u32;
        // local file header
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // stored
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0u16.to_le_bytes()); // date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes()); // compressed
        out.extend_from_slice(&size.to_le_bytes()); // uncompressed
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        // central directory header
        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&0u16.to_le_bytes()); // stored
        central.extend_from_slice(&0u16.to_le_bytes()); // time
        central.extend_from_slice(&0u16.to_le_bytes()); // date
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&name_len.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes()); // disk
    out.extend_from_slice(&0u16.to_le_bytes()); // cd disk
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    out
}

fn write_apk(dir: &Path, name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create apk");
    f.write_all(&zip_bytes(entries)).expect("write apk");
    f.flush().expect("flush");
    path
}

// -------------------------------------------------------------------- cases

#[test]
fn x86_64_libraries_are_installed_into_the_per_run_tree() {
    let dir = temp_dir("install");
    let apk = write_apk(
        &dir,
        "ok.apk",
        &[
            ("lib/x86_64/libfoo.so", b"\x7fELF x86_64 foo"),
            ("lib/x86_64/libbar.so", b"\x7fELF x86_64 bar"),
            ("lib/arm64-v8a/libfoo.so", b"\x7fELF arm64 foo"),
            ("classes.dex", b"dex\n035\0"),
        ],
    );
    let dest = dir.join("app-lib");
    let got = native_libs::install(&apk, &dest).expect("install");
    match got {
        NativeLibInstall::Installed { dir: libdir, libs } => {
            assert_eq!(libdir, dest.join("x86_64"));
            assert_eq!(libs, vec!["libbar.so", "libfoo.so"]);
            assert_eq!(
                std::fs::read(libdir.join("libfoo.so")).expect("read"),
                b"\x7fELF x86_64 foo"
            );
            // The other ABI is never installed on a host run.
            assert!(!dest.join("arm64-v8a").exists());
        }
        other => panic!("expected Installed, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn traversal_entries_are_never_installed_or_written_outside_dest() {
    let dir = temp_dir("traversal");
    let outside = dir.join("evil.so");
    let apk = write_apk(
        &dir,
        "hostile.apk",
        &[
            ("lib/x86_64/../evil.so", b"\x7fELF traversal"),
            ("lib/x86_64/sub/evil.so", b"\x7fELF nested"),
            ("lib/../lib/x86_64/evil.so", b"\x7fELF abi-traversal"),
            ("lib/x86_64/libok.so", b"\x7fELF ok"),
        ],
    );
    let dest = dir.join("app-lib");
    let got = native_libs::install(&apk, &dest).expect("install");
    match got {
        NativeLibInstall::Installed { libs, .. } => assert_eq!(libs, vec!["libok.so"]),
        other => panic!("expected Installed with only libok.so, got {other:?}"),
    }
    assert!(!outside.exists(), "traversal entry escaped the destination");
    assert!(!dir.join("sub").exists());
    assert!(!dest.join("evil.so").exists());
    assert!(!dest.join("x86_64/evil.so").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_arm64_only_apk_is_structured_unsupported_abi() {
    let dir = temp_dir("abi");
    let apk = write_apk(
        &dir,
        "arm.apk",
        &[
            ("lib/arm64-v8a/libfoo.so", b"\x7fELF arm64"),
            ("lib/armeabi-v7a/libfoo.so", b"\x7fELF arm32"),
        ],
    );
    let dest = dir.join("app-lib");
    let got = native_libs::install(&apk, &dest).expect("decision");
    match got {
        NativeLibInstall::UnsupportedAbi {
            available,
            sample,
            message,
        } => {
            assert_eq!(available, vec!["arm64-v8a", "armeabi-v7a"]);
            assert_eq!(
                sample,
                vec![
                    "lib/arm64-v8a/libfoo.so",
                    "lib/armeabi-v7a/libfoo.so"
                ]
            );
            // The message names the host ABI and every ABI the APK does ship; the
            // `UNSUPPORTED_ABI` code itself is the variant (and the launcher's
            // `code`), so it is not repeated in the prose.
            assert!(
                message.contains("x86_64") && message.contains("arm64-v8a"),
                "{message}"
            );
        }
        other => panic!("expected UnsupportedAbi, got {other:?}"),
    }
    assert!(!dest.exists() || std::fs::read_dir(&dest).expect("dest").next().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_apk_without_native_code_installs_nothing() {
    let dir = temp_dir("none");
    let apk = write_apk(&dir, "plain.apk", &[("classes.dex", b"dex\n035\0")]);
    let dest = dir.join("app-lib");
    assert_eq!(
        native_libs::install(&apk, &dest).expect("install"),
        NativeLibInstall::None
    );
    assert!(!dest.exists());
    let _ = std::fs::remove_dir_all(&dir);
}
