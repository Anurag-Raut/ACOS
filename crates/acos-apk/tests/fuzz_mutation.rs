//! Seeded single-byte mutation fuzzing of the AXML and ARSC parsers.
//!
//! The assertion is simply "no panic": any panic aborts the test. A panic-free
//! parser is a stated requirement for ACOS ingest (malformed APKs must produce
//! structured errors, never crashes).

use std::path::PathBuf;

use acos_apk::arsc::ResourceTable;
use acos_apk::axml;
use acos_apk::manifest::Manifest;
use acos_apk::Apk;

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// Minimal deterministic xorshift64* PRNG so the test is reproducible without
/// adding a dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

fn fuzz_bytes(label: &str, base: &[u8], seed: u64, iterations: usize) {
    let mut rng = Rng::new(seed);
    for _ in 0..iterations {
        let mut mutated = base.to_vec();
        if mutated.is_empty() {
            continue;
        }
        let idx = (rng.next_u64() as usize) % mutated.len();
        let delta = (rng.next_u64() as u8) | 1;
        if let Some(b) = mutated.get_mut(idx) {
            *b ^= delta;
        }

        // AXML: parse the DOM and (if it parses) the typed manifest.
        if let Ok(doc) = axml::parse(&mutated) {
            let _ = Manifest::parse(&doc);
        }

        // ARSC.
        let _ = ResourceTable::parse(&mutated);

        let _ = label;
    }
}

#[test]
fn mutation_fuzz_manifest_and_arsc() {
    for name in ["hello-views.apk", "hello-jni.apk"] {
        let apk = Apk::open(data(name)).expect("open fixture");
        let manifest = apk.manifest_raw.clone().expect("manifest");
        fuzz_bytes(name, &manifest, 0x9E37_79B9_7F4A_7C15, 5000);

        if let Some(arsc) = apk.arsc_raw.clone() {
            fuzz_bytes(name, &arsc, 0xC2B2_AE3D_27D4_EB4F, 5000);
        }
    }
}

#[test]
fn mutation_fuzz_random_bytes_do_not_panic() {
    let mut rng = Rng::new(0xDEAD_BEEF_CAFE_F00D);
    for _ in 0..2000 {
        let len = (rng.next_u64() as usize) % 512 + 1;
        let mut buf = vec![0u8; len];
        for b in buf.iter_mut() {
            *b = rng.next_u64() as u8;
        }
        let _ = axml::parse(&buf);
        let _ = ResourceTable::parse(&buf);
    }
}
