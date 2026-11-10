//! Parser fuzzing: seeded mutations and truncations of a real library must
//! never panic — they return errors.

mod common;

use acos_native_loader::dyninfo::DynInfo;
use acos_native_loader::elf::Elf;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn parser_survives_seeded_mutations_and_truncations() {
    let base = std::fs::read(common::data("libhellojni.so")).unwrap();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);

    for i in 0..20_000u64 {
        let mut bytes = base.clone();
        if i % 3 == 0 {
            let n = (rng.next() as usize) % bytes.len();
            bytes.truncate(n);
        } else {
            let n = 1 + (rng.next() as usize) % 16;
            for _ in 0..n {
                let idx = (rng.next() as usize) % bytes.len();
                bytes[idx] = rng.next() as u8;
            }
        }
        // Must return Ok or Err — never panic.
        if let Ok(elf) = Elf::parse(&bytes) {
            let _ = DynInfo::parse(&elf);
        }
    }
}

#[test]
fn parser_rejects_empty_and_tiny_inputs() {
    for len in 0..64usize {
        let bytes = vec![0u8; len];
        assert!(Elf::parse(&bytes).is_err(), "len {len} should not parse");
    }
}
