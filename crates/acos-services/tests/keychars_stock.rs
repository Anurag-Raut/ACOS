//! W16: the stock API-34 `KeyCharacterMap` files the sysroot ships, parsed into
//! the image's native parcel, and the proof that live `type_text` can resolve
//! every character the injector emits.
//!
//! The fixture is the real `/system/usr/keychars/{Virtual,Generic}.kcm` under
//! `.cache/art/sysroot` (exported by `tools/art-sysroot.sh` from the API-34
//! image), or the same files from another configured sysroot. A missing sysroot
//! is a hard failure: guest text input without it is the W13 wall.

use std::path::PathBuf;

use acos_services::keychars::{self, KeyMap};
use acos_services::window_session::input_dispatch::{text_strokes, META_SHIFT_ON};

fn sysroot() -> PathBuf {
    if let Some(dir) = std::env::var_os("ACOS_KEYCHARS_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(root) = std::env::var_os("ACOS_ART_SYSROOT") {
        return PathBuf::from(root).join("system/usr/keychars");
    }
    acos_services::workspace_path(".cache/art/sysroot/system/usr/keychars").expect("workspace root")
}

fn stock(name: &str) -> KeyMap {
    let dir = sysroot();
    let path = dir.join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run tools/art-sysroot.sh)", path.display()));
    keychars::parse(&text, &format!("/system/usr/keychars/{name}"))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// The virtual keyboard's stock map is the one the synthetic `-1` device
/// carries.
#[test]
fn the_virtual_keyboard_map_is_the_stock_full_qwerty() {
    let map = stock("Virtual.kcm");
    assert_eq!(map.keyboard_type, 4, "type FULL");
    assert!(map.keys.len() > 60, "parsed {} keys", map.keys.len());
    for (key, plain, shifted) in [
        (29, 'a', 'A'),  // A
        (30, 'b', 'B'),  // B
        (54, 'z', 'Z'),  // Z
        (7, '0', ')'),   // 0
        (16, '9', '('),  // 9
        (62, ' ', ' '),  // SPACE
        (76, '/', '?'),  // SLASH
        (55, ',', '<'),  // COMMA
        (56, '.', '>'),  // PERIOD
        (75, '\'', '"'), // APOSTROPHE
        (73, '\\', '|'), // BACKSLASH
        (68, '`', '~'),  // GRAVE
        (69, '-', '_'),  // MINUS
        (70, '=', '+'),  // EQUALS
        (71, '[', '{'),  // LEFT_BRACKET
        (72, ']', '}'),  // RIGHT_BRACKET
        (74, ';', ':'),  // SEMICOLON
    ] {
        assert_eq!(map.character(key, 0), Some(plain), "key {key} plain");
        assert_eq!(
            map.character(key, META_SHIFT_ON),
            Some(shifted),
            "key {key} shift"
        );
    }
    assert_eq!(map.character(61, 0), Some('\t'), "TAB");
    assert_eq!(map.character(66, 0), Some('\n'), "ENTER");
}

/// The injector and the stock map agree on every character `type_text` accepts:
/// the key code/meta pair the host publishes resolves to that exact character
/// through the map the guest will hold. This is the host half of the wall; the
/// live run proves the guest half.
#[test]
fn every_injectable_ascii_character_resolves_through_the_stock_map() {
    let map = stock("Virtual.kcm");
    let supported = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 \
                     \n\t-=[]\\;',./`~!@#$%^&*()_+{}|:\"<>?";
    for ch in supported.chars() {
        let strokes = text_strokes(&ch.to_string()).expect("injectable");
        assert_eq!(strokes.len(), 1);
        let (code, meta) = strokes[0];
        assert_eq!(
            map.character(code, meta),
            Some(ch),
            "U+{:04X} publishes key {code} meta {meta:#x}, which the stock map does not resolve",
            ch as u32
        );
    }
}

/// The native parcel from the stock map is consumed exactly, field for field, by
/// a reader written to `KeyCharacterMap::readFromParcel` (`libs/input/
/// KeyCharacterMap.cpp`) and `nativeReadFromParcel` (`android_view_
/// KeyCharacterMap.cpp`): deviceId, hasMap, a **length-less** C string, type,
/// overlay flag, keys with behaviour lists, then the three remap tables.
#[test]
fn the_stock_parcel_is_consumed_by_the_images_native_reader_shape() {
    for (virtual_keyboard, name) in [(true, "Virtual.kcm"), (false, "Generic.kcm")] {
        let map = stock(name);
        let bytes = map.parcel_bytes(if virtual_keyboard { -1 } else { 0 });
        assert_eq!(bytes.len() % 4, 0, "{name}: 4-byte aligned");
        let mut reader = NativeReader::new(&bytes);
        assert_eq!(reader.i32(), if virtual_keyboard { -1 } else { 0 });
        assert_eq!(reader.i32(), 1, "hasMap");
        assert_eq!(
            reader.cstring(),
            format!("/system/usr/keychars/{name}"),
            "writeCString has no length prefix"
        );
        assert_eq!(reader.i32(), 4, "FULL");
        assert_eq!(reader.i32(), 0, "no overlay");
        let num_keys = reader.i32();
        assert_eq!(num_keys as usize, map.keys.len());
        let mut seen = Vec::new();
        for _ in 0..num_keys {
            let key_code = reader.i32();
            let label = reader.i32();
            let number = reader.i32();
            let key = map
                .keys
                .iter()
                .find(|k| k.key_code == key_code)
                .expect("key present");
            assert_eq!(label, i32::from(key.label), "key {key_code} label");
            assert_eq!(number, i32::from(key.number), "key {key_code} number");
            let mut behaviors = Vec::new();
            while reader.i32() == 1 {
                let meta = reader.i32();
                let character = reader.i32();
                let fallback = reader.i32();
                let replacement = reader.i32();
                behaviors.push((meta, character, fallback, replacement));
            }
            let expected: Vec<_> = key
                .behaviors
                .iter()
                .map(|b| {
                    (
                        b.meta_state,
                        i32::from(b.character),
                        b.fallback_key_code,
                        b.replacement_key_code,
                    )
                })
                .collect();
            assert_eq!(behaviors, expected, "key {key_code} behaviours");
            assert!(seen.last().is_none_or(|last| *last < key_code), "ascending");
            seen.push(key_code);
        }
        assert_eq!(reader.i32(), 0, "{name}: mKeyRemapping");
        assert_eq!(reader.i32(), 0, "{name}: mKeysByScanCode");
        assert_eq!(reader.i32(), 0, "{name}: mKeysByUsageCode");
        assert!(reader.done(), "{name}: consumed {} bytes", bytes.len());
    }
}

/// Falling back to a hand-written layout is not allowed: an absent stock file
/// leaves the device with `hasMap = false`, which the stock native reader turns
/// into a map with no keys (never wrong characters).
#[test]
fn a_missing_stock_map_encodes_has_map_false() {
    let bytes = acos_services::keychars::null_parcel(-1);
    assert_eq!(bytes.len(), 8);
    assert_eq!(i32::from_le_bytes(bytes[0..4].try_into().unwrap()), -1);
    assert_eq!(i32::from_le_bytes(bytes[4..8].try_into().unwrap()), 0);
}

struct NativeReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> NativeReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn i32(&mut self) -> i32 {
        let v = i32::from_le_bytes(self.bytes[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        v
    }
    fn cstring(&mut self) -> &'a str {
        let start = self.pos;
        while self.bytes[self.pos] != 0 {
            self.pos += 1;
        }
        let s = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap();
        self.pos += 1;
        while self.pos % 4 != 0 {
            self.pos += 1;
        }
        s
    }
    fn done(&self) -> bool {
        self.pos == self.bytes.len()
    }
}
