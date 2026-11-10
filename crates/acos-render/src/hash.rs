//! Fast frame content hashing.
//!
//! A 128-bit xxh3 hash of the presented pixel buffer is cheap (single pass,
//! SIMD) and stable; it is what `capture()` reports so an agent can tell
//! whether two frames are actually different without diffing images. An
//! optional SHA-256 is available for tamper-evident evidence bundles.

use std::io::Read;

/// Hash RGBA8 bytes to a 128-bit value.
pub fn hash_pixels(data: &[u8]) -> u128 {
    xxhash_rust::xxh3::xxh3_128(data)
}

/// Hex-encoded content hash.
pub fn hash_hex(hash: u128) -> String {
    format!("{hash:032x}")
}

/// Optional cryptographic digest for evidence bundles.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// SHA-256 of a reader (used to hash font/image files for provenance).
pub fn sha256_reader<R: Read>(mut r: R) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_distinguishes() {
        let a = hash_pixels(&[0, 1, 2, 3]);
        let b = hash_pixels(&[0, 1, 2, 3]);
        let c = hash_pixels(&[4, 5, 6, 7]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(hash_hex(a).len(), 32);
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
