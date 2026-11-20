#![allow(clippy::field_reassign_with_default)]
//! Robustness: mutation fuzzing of ARSC and AXML inputs must never panic.
//!
//! A deterministic xorshift PRNG drives byte flips, truncation, zeroing and
//! random splices; every mutated buffer is fed to the parsers and to a few
//! lookups. The test fails only if a parser panics (or returns a value that
//! then panics in a downstream accessor).

use std::path::PathBuf;

use acos_framework_natives::res::arsc::ResTableIndex;
use acos_framework_natives::res::config::ResConfig;
use acos_framework_natives::res::strpool::StringPool;
use acos_framework_natives::res::xmlblock::{parse_axml, XmlBlock};
use acos_framework_natives::res::AssetSource;

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

fn exercise_table(data: &[u8]) {
    if let Ok(table) = ResTableIndex::parse(data) {
        let config = ResConfig::default();
        // A spread of ids incl. out-of-range and framework/app ranges.
        for id in [
            0x0101_0031u32,
            0x0104_0000,
            0x0103_013f,
            0x7f03_0000,
            0x0000_0000,
            0xffff_ffff,
            0x0115_0000,
        ] {
            let _ = table.all_configs(data, id);
            let _ = table.resolve(data, id, &config);
            let _ = table.entry_name(data, id);
            let _ = table.type_name(data, id);
            let _ = table.size_configurations(data);
        }
        if let Ok(pool) = table.global_strings(data) {
            for i in 0..pool.len().min(64) {
                let _ = pool.get_usize(i);
                let _ = pool.get_style(i as i32);
            }
        }
    }
}

fn exercise_axml(data: &[u8]) {
    if let Ok(doc) = parse_axml(data) {
        let block = XmlBlock {
            data: data.to_vec(),
            doc,
        };
        let mut state = acos_framework_natives::res::XmlParserState::new(0, 0);
        for _ in 0..1000 {
            let ev = state.native_next(&block.doc);
            if ev == acos_framework_natives::res::END_DOCUMENT {
                break;
            }
            let _ = state.get_name(&block.doc);
            let _ = state.get_namespace(&block.doc);
            let _ = state.get_text(&block.doc);
            let n = state.get_attribute_count(&block.doc);
            for i in 0..n.min(64) {
                let _ = state.get_attribute_name(&block.doc, i);
                let _ = state.get_attribute_data(&block.doc, i);
                let _ = state.get_attribute_string_value(&block.doc, i);
            }
        }
    }
}

fn mutate(prng: &mut XorShift, original: &[u8], rounds: usize) {
    for round in 0..rounds {
        let mut buf = original.to_vec();
        let strategy = round % 5;
        match strategy {
            0 => {
                // single byte flip
                if !buf.is_empty() {
                    let i = prng.below(buf.len());
                    buf[i] ^= (prng.next() & 0xff) as u8;
                }
            }
            1 => {
                // truncate
                let n = prng.below(buf.len().max(1));
                buf.truncate(n);
            }
            2 => {
                // zero a region
                if !buf.is_empty() {
                    let start = prng.below(buf.len());
                    let len = prng.below(buf.len() - start + 1);
                    for b in &mut buf[start..start + len] {
                        *b = 0;
                    }
                }
            }
            3 => {
                // random splices
                if buf.len() > 8 {
                    for _ in 0..16 {
                        let i = prng.below(buf.len());
                        buf[i] = (prng.next() & 0xff) as u8;
                    }
                }
            }
            _ => {
                // big value words
                if buf.len() >= 4 {
                    let i = prng.below(buf.len() - 3);
                    let v = (prng.next() as u32).to_le_bytes();
                    buf[i..i + 4].copy_from_slice(&v);
                }
            }
        }
        exercise_table(&buf);
        exercise_axml(&buf);
    }
}

#[test]
fn fuzz_arsc_and_axml_never_panic() {
    let mut prng = XorShift(0x9E37_79B9_7F4A_7C15);
    let mut sources: Vec<Vec<u8>> = Vec::new();

    // A real resources.arsc and a real AXML.
    if acos_test_support::require_prereq(
        acos_test_support::have_fixture("fixtures/build/apps/hello-views.apk"),
        "fuzz_arsc_and_axml_never_panic",
        "fixtures/build/apps/hello-views.apk is absent — run tools/build-apps.sh",
    )
    .is_none()
    {
        return;
    }
    let apk_path = workspace_root().join("fixtures/build/apps/hello-views.apk");
    if let Ok(apk) = acos_framework_natives::res::ApkAssets::open(&apk_path) {
        if let Some(arsc) = apk.resource_bytes() {
            sources.push(arsc.to_vec());
        }
        if let Ok(xml) = apk.read_entry("res/layout/main.xml") {
            sources.push(xml);
        }
    }
    // Always include a synthetic table so the test runs with no fixtures.
    sources.push(vec![0u8; 256]);
    sources.push(vec![0x02, 0x00, 0x0c, 0x00]);

    for source in &sources {
        mutate(&mut prng, source, 1500);
    }

    // Also fuzz a standalone string pool header.
    let mut header = Vec::new();
    header.extend_from_slice(&1u16.to_le_bytes());
    header.extend_from_slice(&28u16.to_le_bytes());
    header.extend_from_slice(&32u32.to_le_bytes());
    header.extend_from_slice(&1u32.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    header.extend_from_slice(&28u32.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    header.extend_from_slice(&[0xff; 16]);
    for _ in 0..500 {
        let mut buf = header.clone();
        let i = prng.below(buf.len());
        buf[i] ^= (prng.next() & 0xff) as u8;
        let _ = StringPool::parse(&buf, 0);
    }
    eprintln!("fuzz: completed without panic");
}
