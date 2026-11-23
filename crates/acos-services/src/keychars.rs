//! Stock `KeyCharacterMap` files -> the image's native `KeyCharacterMap` parcel.
//!
//! On API 34 `KeyCharacterMap.load(deviceId)` never opens a `.kcm` file: it
//! returns `InputDevice.getKeyCharacterMap()`, and that map is built by the
//! image's `KeyCharacterMap.nativeReadFromParcel` from the bytes the
//! `IInputManager.getInputDevice` reply carries. The host therefore has to
//! produce the same payload a real `InputManagerService` would have produced
//! after loading `/system/usr/keychars/*.kcm`.
//!
//! The wire form is `libs/input/KeyCharacterMap.cpp`
//! (`readFromParcel`/`writeToParcel`, both compiled into the image):
//!
//! ```text
//! int32  deviceId            (the enclosing InputDevice's id; -1 = virtual)
//! int32  hasMap              (0 -> no native map at all)
//! cstring loadFileName       (raw bytes + NUL, 4-byte padded; NOT a String8)
//! int32  keyboardType        (KeyCharacterMap::{UNKNOWN=0, NUMERIC=1,
//!                             PREDICTIVE=2, ALPHA=3, FULL=4})
//! bool   layoutOverlayApplied
//! int32  numKeys             (then, per key, in ascending keyCode order:
//!                             keyCode, label, number, then
//!                             {1, metaState, character, fallbackKeyCode,
//!                              replacementKeyCode}* terminated by 0)
//! int32  numKeyRemapping     (fromKeyCode, toKeyCode)*
//! int32  numKeysByScanCode   (scanCode, keyCode)*
//! int32  numKeysByUsageCode  (usageCode, keyCode)*
//! ```
//!
//! The map content comes from the stock file shipped in the sysroot
//! (`tools/fetch/art-sysroot.sh` exports `/system/usr/keychars`), parsed with the
//! semantics of the stock `KeyCharacterMap::Parser` (property modifiers,
//! `push_front` behaviour order, default `number` derivation). No hand-written
//! US layout is involved: `/system/usr/keychars/Virtual.kcm` is the map the
//! virtual keyboard device uses on this image, and `Generic.kcm` is the
//! fallback for the synthetic touchscreen.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// `AMETA_SHIFT_ON` (`<android/keycodes.h>`, `KeyEvent.META_SHIFT_ON`).
const META_SHIFT_ON: i32 = 0x0000_0001;
/// `AMETA_ALT_ON`.
const META_ALT_ON: i32 = 0x0000_0002;
/// `AMETA_SYM_ON`.
const META_SYM_ON: i32 = 0x0000_0004;
/// `AMETA_FUNCTION_ON`.
const META_FUNCTION_ON: i32 = 0x0000_0008;
/// `AMETA_ALT_LEFT_ON`.
const META_ALT_LEFT_ON: i32 = 0x0000_0010;
/// `AMETA_ALT_RIGHT_ON`.
const META_ALT_RIGHT_ON: i32 = 0x0000_0020;
/// `AMETA_SHIFT_LEFT_ON`.
const META_SHIFT_LEFT_ON: i32 = 0x0000_0040;
/// `AMETA_SHIFT_RIGHT_ON`.
const META_SHIFT_RIGHT_ON: i32 = 0x0000_0080;
/// `AMETA_CTRL_ON`.
const META_CTRL_ON: i32 = 0x0000_1000;
/// `AMETA_CTRL_LEFT_ON`.
const META_CTRL_LEFT_ON: i32 = 0x0000_2000;
/// `AMETA_CTRL_RIGHT_ON`.
const META_CTRL_RIGHT_ON: i32 = 0x0000_4000;
/// `AMETA_META_ON`.
const META_META_ON: i32 = 0x0001_0000;
/// `AMETA_META_LEFT_ON`.
const META_META_LEFT_ON: i32 = 0x0002_0000;
/// `AMETA_META_RIGHT_ON`.
const META_META_RIGHT_ON: i32 = 0x0004_0000;
/// `AMETA_CAPS_LOCK_ON`.
const META_CAPS_LOCK_ON: i32 = 0x0010_0000;
/// `AMETA_NUM_LOCK_ON`.
const META_NUM_LOCK_ON: i32 = 0x0020_0000;
/// `AMETA_SCROLL_LOCK_ON`.
const META_SCROLL_LOCK_ON: i32 = 0x0040_0000;

/// One `KeyCharacterMap::Behavior`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Behavior {
    /// Meta keys that must be active for this behaviour.
    pub meta_state: i32,
    /// UTF-16 character, `0` for a fallback/replacement-only behaviour.
    pub character: u16,
    /// Key code sent when the app does not handle the key (`0` = none).
    pub fallback_key_code: i32,
    /// Key code that replaces the event outright (`0` = none).
    pub replacement_key_code: i32,
}

/// One `KeyCharacterMap::Key`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    /// Android key code (`AKEYCODE_*`).
    pub key_code: i32,
    /// Printed label (single UTF-16 unit, `0` = none).
    pub label: u16,
    /// Dial-pad number/symbol (single UTF-16 unit, `0` = none).
    pub number: u16,
    /// Behaviours in the native map's order (the parser `push_front`s).
    pub behaviors: Vec<Behavior>,
}

/// A parsed `.kcm` file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyMap {
    /// `/system/usr/keychars/<name>` the map claims to come from.
    pub load_file: String,
    /// `KeyCharacterMap::KeyboardType` (4 = `FULL`).
    pub keyboard_type: i32,
    /// Keys sorted by key code (the native `std::map` order on the wire).
    pub keys: Vec<Key>,
}

impl KeyMap {
    /// The image's native `KeyCharacterMap` parcel for this map, embedded in an
    /// `InputDevice` with `device_id`.
    ///
    /// `device_id` is the enclosing device's id (`-1` for the virtual
    /// keyboard), exactly what `nativeWriteToParcel` writes first.
    #[must_use]
    pub fn parcel_bytes(&self, device_id: i32) -> Vec<u8> {
        let mut out = Vec::with_capacity(4096);
        put_i32(&mut out, device_id);
        put_i32(&mut out, 1); // nativeReadFromParcel: hasMap
        put_cstring(&mut out, &self.load_file);
        put_i32(&mut out, self.keyboard_type);
        put_i32(&mut out, 0); // mLayoutOverlayApplied
        put_i32(&mut out, count_i32(self.keys.len()));
        for key in &self.keys {
            put_i32(&mut out, key.key_code);
            put_i32(&mut out, i32::from(key.label));
            put_i32(&mut out, i32::from(key.number));
            for behavior in &key.behaviors {
                put_i32(&mut out, 1);
                put_i32(&mut out, behavior.meta_state);
                put_i32(&mut out, i32::from(behavior.character));
                put_i32(&mut out, behavior.fallback_key_code);
                put_i32(&mut out, behavior.replacement_key_code);
            }
            put_i32(&mut out, 0);
        }
        put_i32(&mut out, 0); // mKeyRemapping
        put_i32(&mut out, 0); // mKeysByScanCode
        put_i32(&mut out, 0); // mKeysByUsageCode
        out
    }

    /// The behaviour the stock map would select for `key_code`/`meta_state`.
    ///
    /// This mirrors `KeyCharacterMap::getKeyBehavior` + `matchesMetaState`; it
    /// is what the host-side unit tests use to prove the parsed map (and not
    /// only its bytes) has the expected characters.
    #[must_use]
    pub fn character(&self, key_code: i32, meta_state: i32) -> Option<char> {
        let key = self.keys.iter().find(|k| k.key_code == key_code)?;
        let behavior = key
            .behaviors
            .iter()
            .find(|b| matches_meta_state(meta_state, b.meta_state))?;
        char::from_u32(u32::from(behavior.character))
    }
}

fn count_i32(len: usize) -> i32 {
    i32::try_from(len).unwrap_or(i32::MAX)
}

fn put_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// `Parcel::writeCString` (`libbinder`): raw bytes + NUL, 4-byte padded, with
/// **no** length prefix (`readCString` scans for the NUL).
fn put_cstring(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(value.as_bytes());
    out.push(0);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

/// `KeyCharacterMap::matchesMetaState`: the behaviour must be a subset of the
/// event's meta state, and ctrl/alt/meta must match exactly (left/right counts
/// for the generic bit).
fn matches_meta_state(event: i32, behavior: i32) -> bool {
    if event & behavior != behavior {
        return false;
    }
    const EXACT: i32 = META_CTRL_ON
        | META_CTRL_LEFT_ON
        | META_CTRL_RIGHT_ON
        | META_ALT_ON
        | META_ALT_LEFT_ON
        | META_ALT_RIGHT_ON
        | META_META_ON
        | META_META_LEFT_ON
        | META_META_RIGHT_ON;
    let mut unmatched = event & !behavior & EXACT;
    if behavior & META_CTRL_ON != 0 {
        unmatched &= !(META_CTRL_LEFT_ON | META_CTRL_RIGHT_ON);
    } else if behavior & (META_CTRL_LEFT_ON | META_CTRL_RIGHT_ON) != 0 {
        unmatched &= !META_CTRL_ON;
    }
    if behavior & META_ALT_ON != 0 {
        unmatched &= !(META_ALT_LEFT_ON | META_ALT_RIGHT_ON);
    } else if behavior & (META_ALT_LEFT_ON | META_ALT_RIGHT_ON) != 0 {
        unmatched &= !META_ALT_ON;
    }
    if behavior & META_META_ON != 0 {
        unmatched &= !(META_META_LEFT_ON | META_META_RIGHT_ON);
    } else if behavior & (META_META_LEFT_ON | META_META_RIGHT_ON) != 0 {
        unmatched &= !META_META_ON;
    }
    unmatched == 0
}

/// Parse a stock `.kcm` file. `load_file` is the device path recorded in the
/// map (the native reader stores it, it does not reopen the file).
///
/// Unknown syntax is a structured `Err`, never a silently empty map: a typo in
/// a shipped key map must fail loudly instead of producing ``text'' that does
/// not type.
pub fn parse(text: &str, load_file: &str) -> Result<KeyMap, String> {
    let mut map = KeyMap {
        load_file: load_file.to_string(),
        keyboard_type: 0,
        keys: Vec::new(),
    };
    let mut current: Option<Key> = None;
    for (index, raw) in text.lines().enumerate() {
        let lineno = index + 1;
        let line = strip_comment(raw);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(mut key) = current.take() {
            if line == "}" {
                finish_key(&mut key);
                if map.keys.iter().any(|k| k.key_code == key.key_code) {
                    return Err(format!(
                        "{load_file}:{lineno}: duplicate key {}",
                        key.key_code
                    ));
                }
                map.keys.push(key);
                continue;
            }
            parse_property(&mut key, line).map_err(|e| format!("{load_file}:{lineno}: {e}"))?;
            current = Some(key);
            continue;
        }
        if let Some(rest) = line.strip_prefix("type ") {
            map.keyboard_type = match rest.trim() {
                "UNKNOWN" => 0,
                "NUMERIC" => 1,
                "PREDICTIVE" => 2,
                "ALPHA" => 3,
                "FULL" => 4,
                "SPECIAL_FUNCTION" => 5,
                "OVERLAY" => 6,
                other => return Err(format!("{load_file}:{lineno}: unknown type `{other}`")),
            };
        } else if let Some(rest) = line.strip_prefix("key ") {
            let name = rest.trim().trim_end_matches('{').trim();
            if !rest.trim().ends_with('{') {
                return Err(format!(
                    "{load_file}:{lineno}: expected `{{` after key `{name}`"
                ));
            }
            let key_code = keycode(name)
                .ok_or_else(|| format!("{load_file}:{lineno}: unknown key code label `{name}`"))?;
            current = Some(Key {
                key_code,
                label: 0,
                number: 0,
                behaviors: Vec::new(),
            });
        } else {
            return Err(format!("{load_file}:{lineno}: unexpected line `{line}`"));
        }
    }
    if let Some(key) = current {
        return Err(format!(
            "{load_file}: unterminated key body for key code {}",
            key.key_code
        ));
    }
    if map.keyboard_type == 0 {
        return Err(format!("{load_file}: no `type` declaration"));
    }
    map.keys.sort_by_key(|k| k.key_code);
    Ok(map)
}

/// Strip a `#` comment while respecting `'…'` character literals and escapes.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quoted = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match b {
            b'\\' if quoted => escaped = true,
            b'\'' => quoted = !quoted,
            b'#' if !quoted => return &line[..i],
            _ => {}
        }
    }
    line
}

fn parse_property(key: &mut Key, line: &str) -> Result<(), String> {
    let (props, body) = line
        .split_once(':')
        .ok_or_else(|| format!("expected `name: value`, got `{line}`"))?;
    let behavior = parse_behavior(body.trim())?;
    for prop in props.split(',') {
        let prop = prop.trim();
        if prop.is_empty() {
            continue;
        }
        if prop == "label" {
            if behavior.character != 0 {
                key.label = behavior.character;
            }
        } else if prop == "number" {
            if behavior.character != 0 {
                key.number = behavior.character;
            }
        } else {
            let meta_state =
                parse_modifier(prop).ok_or_else(|| format!("unknown modifier `{prop}`"))?;
            if key.behaviors.iter().any(|b| b.meta_state == meta_state) {
                return Err(format!(
                    "duplicate behaviour for meta state {meta_state:#x}"
                ));
            }
            // The stock parser `push_front`s every meta behaviour.
            key.behaviors.insert(
                0,
                Behavior {
                    meta_state,
                    ..behavior
                },
            );
        }
    }
    Ok(())
}

/// `KeyCharacterMap::parseModifier`.
fn parse_modifier(token: &str) -> Option<i32> {
    if token == "base" {
        return Some(0);
    }
    let mut combined = 0;
    for part in token.split('+') {
        let meta = match part {
            "shift" => META_SHIFT_ON,
            "lshift" => META_SHIFT_LEFT_ON,
            "rshift" => META_SHIFT_RIGHT_ON,
            "alt" => META_ALT_ON,
            "lalt" => META_ALT_LEFT_ON,
            "ralt" => META_ALT_RIGHT_ON,
            "ctrl" => META_CTRL_ON,
            "lctrl" => META_CTRL_LEFT_ON,
            "rctrl" => META_CTRL_RIGHT_ON,
            "meta" => META_META_ON,
            "lmeta" => META_META_LEFT_ON,
            "rmeta" => META_META_RIGHT_ON,
            "sym" => META_SYM_ON,
            "fn" => META_FUNCTION_ON,
            "capslock" => META_CAPS_LOCK_ON,
            "numlock" => META_NUM_LOCK_ON,
            "scrolllock" => META_SCROLL_LOCK_ON,
            _ => return None,
        };
        if combined & meta != 0 {
            return None;
        }
        combined |= meta;
    }
    if combined == 0 {
        None
    } else {
        Some(combined)
    }
}

fn parse_behavior(body: &str) -> Result<Behavior, String> {
    let mut behavior = Behavior {
        meta_state: 0,
        character: 0,
        fallback_key_code: 0,
        replacement_key_code: 0,
    };
    let mut rest = body.trim();
    // A behaviour may carry both a character and a fallback/replace code:
    // `base: '\n' fallback ENTER` is stock syntax (NUMPAD_ENTER).
    if rest.starts_with('\'') {
        let (literal, tail) = split_char_literal(rest)
            .ok_or_else(|| format!("invalid character literal `{body}`"))?;
        behavior.character = parse_char_literal(literal)
            .ok_or_else(|| format!("invalid character literal `{literal}`"))?;
        rest = tail.trim();
    }
    if rest.is_empty() {
        return Ok(behavior);
    }
    let mut tokens = rest.split_whitespace();
    let first = tokens.next().unwrap_or("");
    match first {
        "none" => {}
        "fallback" => {
            let label = tokens
                .next()
                .ok_or_else(|| "`fallback` without a key code label".to_string())?;
            behavior.fallback_key_code =
                keycode(label).ok_or_else(|| format!("unknown fallback label `{label}`"))?;
        }
        "replace" => {
            let label = tokens
                .next()
                .ok_or_else(|| "`replace` without a key code label".to_string())?;
            behavior.replacement_key_code =
                keycode(label).ok_or_else(|| format!("unknown replace label `{label}`"))?;
        }
        other => return Err(format!("unknown behaviour `{other}`")),
    }
    Ok(behavior)
}

/// Split `'…'` (honouring `\` escapes) from the rest of a behaviour line.
fn split_char_literal(body: &str) -> Option<(&str, &str)> {
    let bytes = body.as_bytes();
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(1) {
        if escaped {
            escaped = false;
            continue;
        }
        match b {
            b'\\' => escaped = true,
            b'\'' => return Some((&body[..=i], &body[i + 1..])),
            _ => {}
        }
    }
    None
}

/// `KeyCharacterMap::parseCharacterLiteral`: `'x'`, `'\n'`, `'\u00e9'`.
fn parse_char_literal(body: &str) -> Option<u16> {
    let rest = body.strip_prefix('\'')?;
    // The stock tokenizer consumes the closing quote; a missing one is a
    // malformed file, not a one-character string.
    if !body.ends_with('\'') || body.len() < 3 {
        return None;
    }
    let mut chars = rest.char_indices();
    let (_, ch) = chars.next()?;
    let value = if ch == '\\' {
        let (idx, esc) = chars.next()?;
        match esc {
            'n' => 0x000a,
            't' => 0x0009,
            'r' => 0x000d,
            'v' => 0x000b,
            'b' => 0x0008,
            'f' => 0x000c,
            '0' => 0x0000,
            '\\' => u32::from('\\'),
            '\'' => u32::from('\''),
            '"' => u32::from('"'),
            'u' => {
                let hex: String = rest[idx + 1..].chars().take(4).collect();
                u32::from_str_radix(&hex, 16).ok()?
            }
            _ => return None,
        }
    } else {
        u32::from(ch)
    };
    u16::try_from(value).ok()
}

/// `KeyCharacterMap::Parser::finishKey`: derive the dial-pad number when the
/// file does not give one.
fn finish_key(key: &mut Key) {
    if key.number != 0 {
        return;
    }
    let mut digit = 0u16;
    let mut symbol = 0u16;
    for behavior in &key.behaviors {
        let ch = char::from_u32(u32::from(behavior.character));
        match ch {
            Some(c @ '0'..='9') => digit = c as u16,
            Some(c @ ('(' | ')' | '#' | '*' | '-' | '+' | ',' | '.' | '\'' | ':' | ';' | '/')) => {
                symbol = c as u16;
            }
            _ => {}
        }
    }
    key.number = if digit != 0 { digit } else { symbol };
}

/// Key code labels used by the stock `Generic.kcm` / `Virtual.kcm` files, in
/// the image's `InputEventLabels.cpp` `KEYCODES_SEQUENCE` order (the numeric
/// values are the `AKEYCODE_*` constants, e.g. `A` = 29, `SPACE` = 62).
#[rustfmt::skip]
static KEYCODE_LABELS: &[(&str, i32)] = &[
    ("0", 7), ("1", 8), ("2", 9), ("3", 10), ("4", 11), ("5", 12), ("6", 13),
    ("7", 14), ("8", 15), ("9", 16), ("A", 29), ("APOSTROPHE", 75), ("AT", 77),
    ("B", 30), ("BACK", 4), ("BACKSLASH", 73), ("BUTTON_1", 188),
    ("BUTTON_10", 197), ("BUTTON_11", 198), ("BUTTON_12", 199),
    ("BUTTON_13", 200), ("BUTTON_14", 201), ("BUTTON_15", 202),
    ("BUTTON_16", 203), ("BUTTON_2", 189), ("BUTTON_3", 190), ("BUTTON_4", 191),
    ("BUTTON_5", 192), ("BUTTON_6", 193), ("BUTTON_7", 194), ("BUTTON_8", 195),
    ("BUTTON_9", 196), ("BUTTON_A", 96), ("BUTTON_B", 97), ("BUTTON_C", 98),
    ("BUTTON_L1", 102), ("BUTTON_L2", 104), ("BUTTON_MODE", 110),
    ("BUTTON_R1", 103), ("BUTTON_R2", 105), ("BUTTON_SELECT", 109),
    ("BUTTON_START", 108), ("BUTTON_THUMBL", 106), ("BUTTON_THUMBR", 107),
    ("BUTTON_X", 99), ("BUTTON_Y", 100), ("BUTTON_Z", 101), ("C", 31),
    ("COMMA", 55), ("D", 32), ("DEL", 67), ("DPAD_CENTER", 23), ("DPAD_DOWN", 20),
    ("DPAD_LEFT", 21), ("DPAD_RIGHT", 22), ("DPAD_UP", 19), ("E", 33),
    ("ENTER", 66), ("EQUALS", 70), ("ESCAPE", 111), ("F", 34),
    ("FORWARD_DEL", 112), ("G", 35), ("GRAVE", 68), ("H", 36), ("HOME", 3),
    ("I", 37), ("INSERT", 124), ("J", 38), ("K", 39), ("L", 40),
    ("LANGUAGE_SWITCH", 204), ("LEFT_BRACKET", 71), ("M", 41), ("MENU", 82),
    ("MINUS", 69), ("MOVE_END", 123), ("MOVE_HOME", 122), ("N", 42),
    ("NUMPAD_0", 144), ("NUMPAD_1", 145), ("NUMPAD_2", 146), ("NUMPAD_3", 147),
    ("NUMPAD_4", 148), ("NUMPAD_5", 149), ("NUMPAD_6", 150), ("NUMPAD_7", 151),
    ("NUMPAD_8", 152), ("NUMPAD_9", 153), ("NUMPAD_ADD", 157),
    ("NUMPAD_COMMA", 159), ("NUMPAD_DIVIDE", 154), ("NUMPAD_DOT", 158),
    ("NUMPAD_ENTER", 160), ("NUMPAD_EQUALS", 161), ("NUMPAD_LEFT_PAREN", 162),
    ("NUMPAD_MULTIPLY", 155), ("NUMPAD_RIGHT_PAREN", 163),
    ("NUMPAD_SUBTRACT", 156), ("O", 43), ("P", 44), ("PAGE_DOWN", 93),
    ("PAGE_UP", 92), ("PERIOD", 56), ("PLUS", 81), ("POUND", 18), ("Q", 45),
    ("R", 46), ("RIGHT_BRACKET", 72), ("S", 47), ("SEARCH", 84),
    ("SEMICOLON", 74), ("SLASH", 76), ("SPACE", 62), ("STAR", 17), ("T", 48),
    ("TAB", 61), ("U", 49), ("V", 50), ("W", 51), ("X", 52), ("Y", 53),
    ("Z", 54),
];

/// `AKEYCODE_*` for a stock key code label.
#[must_use]
pub fn keycode(label: &str) -> Option<i32> {
    KEYCODE_LABELS
        .iter()
        .find(|(name, _)| *name == label)
        .map(|(_, code)| *code)
}

/// The keychars directory: `ACOS_KEYCHARS_DIR`, then the sysroot the launcher
/// resolves, then the productised/feasibility `.cache` locations under
/// `$ACOS_HOME` (REL1a) or the compile-time workspace root, and finally a
/// guest-like `/system`.
#[must_use]
pub fn keychars_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ACOS_KEYCHARS_DIR") {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    if let Some(root) = std::env::var_os("ACOS_ART_SYSROOT") {
        let dir = PathBuf::from(root).join("system/usr/keychars");
        if dir.is_dir() {
            return Some(dir);
        }
    }
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("ACOS_HOME") {
        if !home.is_empty() {
            roots.push(PathBuf::from(home));
        }
    }
    roots.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    for repo in roots {
        for rel in [".cache/art/sysroot", ".cache/art-exp/sysroot"] {
            let cand = repo.join(rel).join("system/usr/keychars");
            if cand.is_dir() {
                return Some(cand);
            }
        }
    }
    let guest = PathBuf::from("/system/usr/keychars");
    guest.is_dir().then_some(guest)
}

/// The two stock maps the synthetic InputDevices use, parsed once.
pub struct StockMaps {
    /// `Virtual.kcm` (the virtual keyboard device, id `-1`).
    pub virtual_keyboard: Option<KeyMap>,
    /// `Generic.kcm` (the touchscreen device, id `0`).
    pub generic: Option<KeyMap>,
}

static STOCK: OnceLock<StockMaps> = OnceLock::new();

fn load_stock() -> StockMaps {
    let Some(dir) = keychars_dir() else {
        eprintln!(
            "acos-services: keychars: no /system/usr/keychars (guest text input will not resolve); \
             run tools/fetch/art-sysroot.sh"
        );
        return StockMaps {
            virtual_keyboard: None,
            generic: None,
        };
    };
    let read = |name: &str| -> Option<KeyMap> {
        let path = dir.join(name);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("acos-services: keychars: {}: {e}", path.display());
                return None;
            }
        };
        match parse(&text, &format!("/system/usr/keychars/{name}")) {
            Ok(map) => Some(map),
            Err(e) => {
                eprintln!("acos-services: keychars: {e}");
                None
            }
        }
    };
    let mut generic = read("Generic.kcm");
    // A real virtual keyboard device carries `Virtual.kcm`; when only the
    // generic file exists, that is the next-best stock map (never a hand
    // written one).
    let virtual_keyboard = read("Virtual.kcm").or_else(|| generic.clone());
    if generic.is_none() {
        generic = virtual_keyboard.clone();
    }
    StockMaps {
        virtual_keyboard,
        generic,
    }
}

/// The stock maps (parsed on first use).
#[must_use]
pub fn stock() -> &'static StockMaps {
    STOCK.get_or_init(load_stock)
}

/// The native payload for a device whose stock map is unavailable:
/// `deviceId` then `hasMap = false`. The image's `nativeReadFromParcel` builds a
/// `KeyCharacterMap` with no native map from this (a map with no keys), which is
/// the honest answer: no characters, never a guessed layout.
#[must_use]
pub fn null_parcel(device_id: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    put_i32(&mut out, device_id);
    put_i32(&mut out, 0);
    out
}

/// The native `KeyCharacterMap` parcel for the virtual keyboard (`true`) or the
/// touchscreen (`false`) device, when the stock map is available.
#[must_use]
pub fn stock_parcel(virtual_keyboard: bool) -> Option<Vec<u8>> {
    let stock = stock();
    let map = if virtual_keyboard {
        stock.virtual_keyboard.as_ref()
    } else {
        stock.generic.as_ref()
    }?;
    Some(map.parcel_bytes(if virtual_keyboard { -1 } else { 0 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keycode_labels_are_the_image_values() {
        assert_eq!(keycode("A"), Some(29));
        assert_eq!(keycode("SPACE"), Some(62));
        assert_eq!(keycode("ENTER"), Some(66));
        assert_eq!(keycode("BUTTON_16"), Some(203));
        assert_eq!(keycode("NOPE"), None);
    }

    #[test]
    fn a_character_map_parses_the_stock_property_shapes() {
        let text = r#"
            type FULL
            key A {
                label:                              'A'
                base:                               'a'
                shift, capslock:                    'A'
                shift+capslock:                     'a'
            }
            key BUTTON_X {
                base:                               fallback DEL
            }
            key SPACE {
                base:                               ' '
            }
        "#;
        let map = parse(text, "/system/usr/keychars/Test.kcm").unwrap();
        assert_eq!(map.keyboard_type, 4);
        assert_eq!(map.character(29, 0), Some('a'));
        assert_eq!(map.character(29, META_SHIFT_ON), Some('A'));
        assert_eq!(map.character(29, META_CAPS_LOCK_ON), Some('A'));
        assert_eq!(
            map.character(29, META_SHIFT_ON | META_CAPS_LOCK_ON),
            Some('a')
        );
        assert_eq!(map.character(62, 0), Some(' '));
        let x = map.keys.iter().find(|k| k.key_code == 99).unwrap();
        assert_eq!(x.behaviors[0].fallback_key_code, 67, "DEL");
        assert_eq!(x.behaviors[0].character, 0);
    }

    #[test]
    fn the_parcel_is_the_native_read_from_parcel_layout() {
        let text = "type FULL\nkey A {\n label: 'A'\n base: 'a'\n shift: 'A'\n}\n";
        let map = parse(text, "/system/usr/keychars/Test.kcm").unwrap();
        let bytes = map.parcel_bytes(-1);
        let mut p = ByteReader::new(&bytes);
        assert_eq!(p.i32(), -1, "deviceId");
        assert_eq!(p.i32(), 1, "hasMap");
        assert_eq!(p.cstring(), "/system/usr/keychars/Test.kcm");
        assert_eq!(p.i32(), 4, "FULL");
        assert_eq!(p.i32(), 0, "no overlay");
        assert_eq!(p.i32(), 1, "one key");
        assert_eq!(p.i32(), 29, "A");
        assert_eq!(p.i32(), u32::from('A') as i32, "label");
        assert_eq!(p.i32(), 0, "no dial number");
        // shift behaviour is pushed in front of base.
        assert_eq!(p.i32(), 1);
        assert_eq!(p.i32(), META_SHIFT_ON);
        assert_eq!(p.i32(), u32::from('A') as i32);
        assert_eq!(p.i32(), 0);
        assert_eq!(p.i32(), 0);
        assert_eq!(p.i32(), 1);
        assert_eq!(p.i32(), 0); // base meta
        assert_eq!(p.i32(), u32::from('a') as i32);
        assert_eq!(p.i32(), 0);
        assert_eq!(p.i32(), 0);
        assert_eq!(p.i32(), 0, "behaviour terminator");
        assert_eq!(p.i32(), 0, "no remap");
        assert_eq!(p.i32(), 0, "no scan-code map");
        assert_eq!(p.i32(), 0, "no usage-code map");
        assert!(p.done(), "parcel consumed exactly: {} bytes", bytes.len());
        assert_eq!(bytes.len() % 4, 0, "native parcels are 4-byte aligned");
    }

    #[test]
    fn unknown_or_truncated_stock_syntax_is_a_structured_error() {
        let e = parse("type FULL\nkey NOPE {\n base: 'a'\n}\n", "x").unwrap_err();
        assert!(e.contains("unknown key code label `NOPE`"), "{e}");
        let e = parse("key A {\n base: 'a'\n}\n", "x").unwrap_err();
        assert!(e.contains("no `type` declaration"), "no type: {e}");
        let e = parse("type FULL\nkey A {\n base: 'a'\n", "x").unwrap_err();
        assert!(e.contains("unterminated key body"), "{e}");
        let e = parse("type FULL\nkey A {\n wat: 'a'\n}\n", "x").unwrap_err();
        assert!(e.contains("unknown modifier `wat`"), "{e}");
        let e = parse("type FULL\nkey A {\n base: fallback NOPE\n}\n", "x").unwrap_err();
        assert!(e.contains("unknown fallback label `NOPE`"), "{e}");
        let e = parse("type FULL\nkey A {\n base: 'a\n}\n", "x").unwrap_err();
        assert!(e.contains("invalid character literal"), "{e}");
    }

    #[test]
    fn the_comment_stripper_leaves_quoted_hashes_alone() {
        assert_eq!(strip_comment("base: '#' # the pound key"), "base: '#' ");
        assert_eq!(strip_comment("base: 'a' # x"), "base: 'a' ");
        assert_eq!(strip_comment("  # all comment"), "  ");
    }

    struct ByteReader<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl<'a> ByteReader<'a> {
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
}
