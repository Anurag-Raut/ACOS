//! MD5 (RFC 1321), in process.
//!
//! `content_hash` is published as the MD5 of the frame's PNG bytes, and the
//! launcher used to obtain it by forking `md5sum` and reading the finished file
//! back: 9.6 ms of one warm screenshot's 30 ms (`results/art-runtime/D-M5.md`
//! rank 4). This module produces the identical hex in process, so the encoder
//! can stream its own output through it (no fork, no second file read) while
//! the value stays byte-identical to what `md5sum` prints for the same bytes.
//!
//! Speed matters here (9.5 MiB per frame), so the 64 rounds are fully unrolled
//! and the message schedule is read as little-endian words. Correctness is
//! pinned by the RFC 1321 test vectors below and by a differential test against
//! the host `md5sum` on a real 9.5 MB frame.

const K: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// One MD5 step: `a = b + rotl(a + f(b,c,d) + x + K[i], s)`.
///
/// The round number selects the nonlinear function, written in the role names
/// `b`, `c`, `d` of *this* step, so the fully unrolled schedule can rotate the
/// roles without changing the text.
macro_rules! md5_step {
    (1, $a:ident, $b:ident, $c:ident, $d:ident, $x:expr, $i:expr) => {
        md5_step!($d ^ ($b & ($c ^ $d)), $a, $b, $c, $d, $x, $i)
    };
    (2, $a:ident, $b:ident, $c:ident, $d:ident, $x:expr, $i:expr) => {
        md5_step!($c ^ ($d & ($b ^ $c)), $a, $b, $c, $d, $x, $i)
    };
    (3, $a:ident, $b:ident, $c:ident, $d:ident, $x:expr, $i:expr) => {
        md5_step!($b ^ $c ^ $d, $a, $b, $c, $d, $x, $i)
    };
    (4, $a:ident, $b:ident, $c:ident, $d:ident, $x:expr, $i:expr) => {
        md5_step!($c ^ ($b | !$d), $a, $b, $c, $d, $x, $i)
    };
    ($f:expr, $a:ident, $b:ident, $c:ident, $d:ident, $x:expr, $i:expr) => {
        $a = $b.wrapping_add(
            $a.wrapping_add($f)
                .wrapping_add($x)
                .wrapping_add(K[$i])
                .rotate_left(S[$i]),
        );
    };
}

/// MD5's compression function over one 64-byte block.
fn compress(state: &mut [u32; 4], block: &[u8]) {
    debug_assert_eq!(block.len(), 64);
    let mut x = [0u32; 16];
    for (word, chunk) in x.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    let (mut a, mut b, mut c, mut d) = (state[0], state[1], state[2], state[3]);
    // Round 1..4, fully unrolled: the role rotation and the message-word index
    // schedule are both compile-time constants; `md5_step!` writes each round's
    // nonlinear function in the *role* names of that step.
    md5_step!(1, a, b, c, d, x[0], 0);
    md5_step!(1, d, a, b, c, x[1], 1);
    md5_step!(1, c, d, a, b, x[2], 2);
    md5_step!(1, b, c, d, a, x[3], 3);
    md5_step!(1, a, b, c, d, x[4], 4);
    md5_step!(1, d, a, b, c, x[5], 5);
    md5_step!(1, c, d, a, b, x[6], 6);
    md5_step!(1, b, c, d, a, x[7], 7);
    md5_step!(1, a, b, c, d, x[8], 8);
    md5_step!(1, d, a, b, c, x[9], 9);
    md5_step!(1, c, d, a, b, x[10], 10);
    md5_step!(1, b, c, d, a, x[11], 11);
    md5_step!(1, a, b, c, d, x[12], 12);
    md5_step!(1, d, a, b, c, x[13], 13);
    md5_step!(1, c, d, a, b, x[14], 14);
    md5_step!(1, b, c, d, a, x[15], 15);
    md5_step!(2, a, b, c, d, x[1], 16);
    md5_step!(2, d, a, b, c, x[6], 17);
    md5_step!(2, c, d, a, b, x[11], 18);
    md5_step!(2, b, c, d, a, x[0], 19);
    md5_step!(2, a, b, c, d, x[5], 20);
    md5_step!(2, d, a, b, c, x[10], 21);
    md5_step!(2, c, d, a, b, x[15], 22);
    md5_step!(2, b, c, d, a, x[4], 23);
    md5_step!(2, a, b, c, d, x[9], 24);
    md5_step!(2, d, a, b, c, x[14], 25);
    md5_step!(2, c, d, a, b, x[3], 26);
    md5_step!(2, b, c, d, a, x[8], 27);
    md5_step!(2, a, b, c, d, x[13], 28);
    md5_step!(2, d, a, b, c, x[2], 29);
    md5_step!(2, c, d, a, b, x[7], 30);
    md5_step!(2, b, c, d, a, x[12], 31);
    md5_step!(3, a, b, c, d, x[5], 32);
    md5_step!(3, d, a, b, c, x[8], 33);
    md5_step!(3, c, d, a, b, x[11], 34);
    md5_step!(3, b, c, d, a, x[14], 35);
    md5_step!(3, a, b, c, d, x[1], 36);
    md5_step!(3, d, a, b, c, x[4], 37);
    md5_step!(3, c, d, a, b, x[7], 38);
    md5_step!(3, b, c, d, a, x[10], 39);
    md5_step!(3, a, b, c, d, x[13], 40);
    md5_step!(3, d, a, b, c, x[0], 41);
    md5_step!(3, c, d, a, b, x[3], 42);
    md5_step!(3, b, c, d, a, x[6], 43);
    md5_step!(3, a, b, c, d, x[9], 44);
    md5_step!(3, d, a, b, c, x[12], 45);
    md5_step!(3, c, d, a, b, x[15], 46);
    md5_step!(3, b, c, d, a, x[2], 47);
    md5_step!(4, a, b, c, d, x[0], 48);
    md5_step!(4, d, a, b, c, x[7], 49);
    md5_step!(4, c, d, a, b, x[14], 50);
    md5_step!(4, b, c, d, a, x[5], 51);
    md5_step!(4, a, b, c, d, x[12], 52);
    md5_step!(4, d, a, b, c, x[3], 53);
    md5_step!(4, c, d, a, b, x[10], 54);
    md5_step!(4, b, c, d, a, x[1], 55);
    md5_step!(4, a, b, c, d, x[8], 56);
    md5_step!(4, d, a, b, c, x[15], 57);
    md5_step!(4, c, d, a, b, x[6], 58);
    md5_step!(4, b, c, d, a, x[13], 59);
    md5_step!(4, a, b, c, d, x[4], 60);
    md5_step!(4, d, a, b, c, x[11], 61);
    md5_step!(4, c, d, a, b, x[2], 62);
    md5_step!(4, b, c, d, a, x[9], 63);
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
}

/// A streaming MD5: feed the exact bytes that go to the file, once.
#[derive(Clone)]
pub struct Md5 {
    state: [u32; 4],
    /// Bytes of the current, not yet compressed block.
    buffer: [u8; 64],
    buffered: usize,
    /// Message length in bytes, so far.
    length: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Md5 {
    pub fn new() -> Self {
        Md5 {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            buffer: [0u8; 64],
            buffered: 0,
            length: 0,
        }
    }

    /// Absorb `data`. Any split of the same byte stream gives the same digest.
    pub fn update(&mut self, data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);
        let mut rest = data;
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(rest.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&rest[..take]);
            self.buffered += take;
            rest = &rest[take..];
            if self.buffered == 64 {
                let block = self.buffer;
                compress(&mut self.state, &block);
                self.buffered = 0;
            }
        }
        let mut blocks = rest.chunks_exact(64);
        for block in &mut blocks {
            compress(&mut self.state, block);
        }
        let tail = blocks.remainder();
        if !tail.is_empty() {
            self.buffer[..tail.len()].copy_from_slice(tail);
            self.buffered = tail.len();
        }
    }

    /// The digest as 32 lowercase hex characters (exactly `md5sum`'s field 1).
    pub fn finish_hex(mut self) -> String {
        let bits = self.length.wrapping_mul(8);
        let mut padding = [0u8; 72];
        padding[0] = 0x80;
        // Pad to 56 mod 64, then the 64-bit little-endian bit length.
        let pad = if self.buffered < 56 {
            56 - self.buffered
        } else {
            120 - self.buffered
        };
        padding[pad..pad + 8].copy_from_slice(&bits.to_le_bytes());
        let length = self.length;
        // `update` would rewrite `length`; pad with the length fixed instead.
        self.length = length;
        let pad = &padding[..pad + 8];
        let mut rest = pad;
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(rest.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&rest[..take]);
            self.buffered += take;
            rest = &rest[take..];
            if self.buffered == 64 {
                let block = self.buffer;
                compress(&mut self.state, &block);
                self.buffered = 0;
            }
        }
        let mut blocks = rest.chunks_exact(64);
        for block in &mut blocks {
            compress(&mut self.state, block);
        }
        debug_assert!(blocks.remainder().is_empty());
        let mut hex = String::with_capacity(32);
        for word in self.state {
            for byte in word.to_le_bytes() {
                hex.push(char::from(HEX[(byte >> 4) as usize]));
                hex.push(char::from(HEX[(byte & 0xf) as usize]));
            }
        }
        hex
    }
}

const HEX: [u8; 16] = *b"0123456789abcdef";

/// MD5 of `data` as 32 lowercase hex characters.
pub fn md5_hex(data: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(data);
    hasher.finish_hex()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 1321, section A.5: the seven published test vectors.
    #[test]
    fn rfc1321_test_vectors() {
        let vectors = [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ];
        for (input, want) in vectors {
            assert_eq!(md5_hex(input.as_bytes()), want, "input {input:?}");
        }
    }

    /// The digest does not depend on how the byte stream is split: this is what
    /// lets the encoder feed its output in arbitrary chunks.
    #[test]
    fn chunking_does_not_change_the_digest() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 31 % 251) as u8).collect();
        let whole = md5_hex(&data);
        for split in [1usize, 3, 7, 55, 56, 57, 63, 64, 65, 127, 128, 500] {
            let mut hasher = Md5::new();
            for chunk in data.chunks(split) {
                hasher.update(chunk);
            }
            assert_eq!(hasher.finish_hex(), whole, "split every {split} bytes");
        }
    }

    /// Every padding boundary (0, 55, 56, 57, 63, 64, 65, 119, 120 bytes).
    #[test]
    fn padding_boundaries_match_the_one_shot_digest() {
        for len in [0usize, 1, 55, 56, 57, 63, 64, 65, 119, 120, 128, 191] {
            let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
            assert_eq!(md5_hex(&data), md5_hex(&data[..]), "len {len}");
            if len > 1 {
                let mut split = Md5::new();
                split.update(&data[..1]);
                split.update(&data[1..]);
                assert_eq!(split.finish_hex(), md5_hex(&data), "split len {len}");
            }
        }
    }
}
