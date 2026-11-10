//! Corruption / robustness tests. Every malformed input must produce a
//! structured error or a successful parse — never a panic.

use acos_dex::hash::{adler32, sha1};
use acos_dex::DexFile;
use std::path::PathBuf;
use std::sync::Arc;

fn fixture() -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/fixtures.dex"),
    )
    .expect("fixture")
}

/// Recompute the Adler-32 checksum and SHA-1 signature so the parser proceeds
/// past header validation into the structural code.
fn fixup(bytes: &mut [u8]) {
    let sig = sha1(&bytes[32..]);
    bytes[12..32].copy_from_slice(&sig);
    let checksum = adler32(&bytes[12..]);
    bytes[8..12].copy_from_slice(&checksum.to_le_bytes());
}

#[test]
fn bad_magic() {
    let mut b = fixture();
    b[0] = b'x';
    let err = DexFile::parse(b).unwrap_err();
    assert_eq!(err.code(), "DEX_BAD_MAGIC");
}

#[test]
fn unsupported_version() {
    let mut b = fixture();
    b[4..7].copy_from_slice(b"999");
    let err = DexFile::parse(b).unwrap_err();
    assert_eq!(err.code(), "DEX_UNSUPPORTED_VERSION");
}

#[test]
fn bad_checksum() {
    let mut b = fixture();
    // Flip a byte in the body without fixing the checksum.
    let mid = b.len() / 2;
    b[mid] ^= 0xff;
    let err = DexFile::parse(b).unwrap_err();
    assert_eq!(err.code(), "DEX_BAD_CHECKSUM");
}

#[test]
fn bad_signature() {
    let mut b = fixture();
    // Change a body byte and recompute only the checksum; signature now stale.
    let mid = b.len() / 2;
    b[mid] ^= 0xff;
    let checksum = adler32(&b[12..]);
    b[8..12].copy_from_slice(&checksum.to_le_bytes());
    let err = DexFile::parse(b).unwrap_err();
    assert_eq!(err.code(), "DEX_BAD_SIGNATURE");
}

#[test]
fn truncated_file() {
    let b = fixture();
    for len in [0usize, 4, 7, 8, 111, 200] {
        let err = DexFile::parse(b[..len.min(b.len())].to_vec()).unwrap_err();
        assert!(
            matches!(err.code(), "DEX_TRUNCATED" | "DEX_BAD_MAGIC"),
            "len {len}: {}",
            err.code()
        );
    }
}

#[test]
fn bad_endian_tag() {
    let mut b = fixture();
    b[40..44].copy_from_slice(&0x7856_3412u32.to_le_bytes());
    fixup(&mut b);
    let err = DexFile::parse(b).unwrap_err();
    assert_eq!(err.code(), "DEX_BAD_ENDIAN");
}

#[test]
fn out_of_range_type_index() {
    let dex = DexFile::parse(fixture()).unwrap();
    let type_ids_off = dex.header().type_ids_off as usize;
    let mut b = fixture();
    b[type_ids_off..type_ids_off + 4].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
    fixup(&mut b);
    let err = DexFile::parse(b).unwrap_err();
    assert_eq!(err.code(), "DEX_INDEX_OUT_OF_RANGE");
}

#[test]
fn out_of_range_string_offset() {
    let dex = DexFile::parse(fixture()).unwrap();
    let string_ids_off = dex.header().string_ids_off as usize;
    let mut b = fixture();
    // Point the first string at a bogus, out-of-file offset.
    b[string_ids_off..string_ids_off + 4].copy_from_slice(&0x7fff_fff0u32.to_le_bytes());
    fixup(&mut b);
    let err = DexFile::parse(b).unwrap_err();
    assert_eq!(err.code(), "DEX_TRUNCATED");
}

#[test]
fn parse_accepts_slice_and_arc() {
    let b = fixture();
    DexFile::parse(b.as_slice()).unwrap();
    DexFile::parse(b.clone()).unwrap();
    let arc: Arc<[u8]> = Arc::from(b.into_boxed_slice());
    DexFile::parse(arc).unwrap();
}

fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

#[test]
fn seeded_single_byte_mutations_never_panic() {
    let base = fixture();
    let mut state = 0x1234_5678_9abc_def0u64;
    let iterations: u64 = std::env::var("ACOS_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let mut ok = 0u64;
    let mut err = 0u64;
    for i in 0..iterations {
        let mut b = base.clone();
        let idx = (xorshift(&mut state) as usize) % b.len();
        let val = xorshift(&mut state) as u8;
        b[idx] = val;
        // Half the time repair the header so parsing reaches deeper structures.
        if i % 2 == 0 {
            fixup(&mut b);
        }
        match DexFile::parse(b) {
            Ok(dex) => {
                ok += 1;
                let _ = dex.verify();
            }
            Err(_) => err += 1,
        }
    }
    assert_eq!(ok + err, iterations);
    // Sanity: the mutation corpus should exercise both outcomes.
    assert!(err > 0, "expected some mutations to fail");
}

#[test]
fn multiple_byte_mutations_never_panic() {
    let base = fixture();
    let mut state = 0xdead_beef_cafe_babeu64;
    for i in 0..1000 {
        let mut b = base.clone();
        for _ in 0..8 {
            let idx = (xorshift(&mut state) as usize) % b.len();
            b[idx] = xorshift(&mut state) as u8;
        }
        if i % 2 == 0 {
            fixup(&mut b);
        }
        if let Ok(dex) = DexFile::parse(b) {
            let _ = dex.verify();
        }
    }
}
