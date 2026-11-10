//! Rust parser for Android key character map (`.kcm`) files.
//!
//! The real `libandroid` `KeyCharacterMap` parses `/system/usr/keychars/*.kcm`
//! and answers `nativeGetCharacter` / `nativeGetFallbackAction` from it. ACOS
//! ships the AOSP generic layout as `keychars/Generic.kcm` and parses it here so
//! the framework's `KeyCharacterMap` natives are backed by real key mappings
//! instead of a hard-coded QWERTY slice.
//!
//! The grammar implemented is the subset the AOSP layout files use:
//!
//! ```text
//! type FULL
//! key A {
//!     label:       'A'
//!     base:        'a'
//!     shift, capslock: 'A'
//!     shift+capslock:  'a'
//!     alt:         fallback SEARCH
//! }
//! ```
//!
//! A `key` name is an Android key-code name without the `KEYCODE_` prefix
//! (`A`, `NUMPAD_0`, `BUTTON_A`, ...); [`key_code_from_name`] is the generated
//! table transcribed from the API-34 `android.view.KeyEvent` constants.

use std::collections::BTreeMap;

/// The AOSP generic full keyboard shipped with ACOS.
pub const GENERIC_KCM: &str = include_str!("keychars/Generic.kcm");

/// `KeyEvent.META_SHIFT_ON`.
pub const META_SHIFT: i32 = 0x1;
/// `KeyEvent.META_ALT_ON`.
pub const META_ALT: i32 = 0x2;
/// `KeyEvent.META_SYM_ON`.
pub const META_SYM: i32 = 0x4;
/// `KeyEvent.META_CTRL_ON`.
pub const META_CTRL: i32 = 0x1000;
/// `KeyEvent.META_META_ON`.
pub const META_META: i32 = 0x10000;
/// `KeyEvent.META_CAPS_LOCK_ON`.
pub const META_CAPS_LOCK: i32 = 0x100000;
/// `KeyEvent.META_NUM_LOCK_ON`.
pub const META_NUM_LOCK: i32 = 0x200000;

/// `KeyCharacterMap.NUMERIC`.
pub const KEYBOARD_NUMERIC: i32 = 1;
/// `KeyCharacterMap.PREDICTIVE`.
pub const KEYBOARD_PREDICTIVE: i32 = 2;
/// `KeyCharacterMap.ALPHA`.
pub const KEYBOARD_ALPHA: i32 = 3;
/// `KeyCharacterMap.FULL`.
pub const KEYBOARD_FULL: i32 = 4;
/// `KeyCharacterMap.SPECIAL_FUNCTION`.
pub const KEYBOARD_SPECIAL_FUNCTION: i32 = 5;

/// One `modifier-combination -> character` entry inside a `key` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Variation {
    /// Modifier mask (`META_*` bits) that must be present.
    pub meta: i32,
    /// The produced character (`0` when this is a pure fallback / `none`).
    pub ch: u16,
    /// `fallback <KEYCODE>` target, when the entry declares one.
    pub fallback: Option<i32>,
    /// The entry was `none` (explicitly produces nothing).
    pub is_none: bool,
}

/// One `key` block.
#[derive(Debug, Clone, Default)]
pub struct Key {
    /// `label:` character.
    pub label: u16,
    /// `number:` character.
    pub number: u16,
    /// Every modifier variant, in file order.
    pub variations: Vec<Variation>,
}

/// A parsed key character map.
#[derive(Debug, Clone)]
pub struct Kcm {
    /// `KeyCharacterMap` keyboard type (`FULL`, `ALPHA`, `NUMERIC`, ...).
    pub keyboard_type: i32,
    /// Key-code to [`Key`] block.
    pub keys: BTreeMap<i32, Key>,
}

impl Default for Kcm {
    fn default() -> Self {
        Self {
            keyboard_type: KEYBOARD_FULL,
            keys: BTreeMap::new(),
        }
    }
}

impl Kcm {
    /// Parse a `.kcm` document.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut kcm = Kcm::default();
        let mut current: Option<(i32, Key)> = None;
        for raw in text.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("type") {
                let t = rest.trim();
                kcm.keyboard_type = match t {
                    "NUMERIC" => KEYBOARD_NUMERIC,
                    "PREDICTIVE" => KEYBOARD_PREDICTIVE,
                    "ALPHA" => KEYBOARD_ALPHA,
                    "FULL" => KEYBOARD_FULL,
                    "SPECIAL_FUNCTION" => KEYBOARD_SPECIAL_FUNCTION,
                    _ => KEYBOARD_FULL,
                };
                continue;
            }
            if let Some(rest) = line.strip_prefix("key") {
                let name = rest.trim().trim_end_matches('{').trim();
                let code = key_code_from_name(name).unwrap_or(0);
                current = Some((code, Key::default()));
                continue;
            }
            if line == "}" {
                if let Some((code, key)) = current.take() {
                    if code != 0 {
                        kcm.keys.insert(code, key);
                    }
                }
                continue;
            }
            let Some((lhs, rhs)) = line.split_once(':') else {
                continue;
            };
            let Some((_, key)) = current.as_mut() else {
                continue;
            };
            let value = parse_value(rhs.trim());
            for combo in lhs.split(',').map(str::trim).filter(|c| !c.is_empty()) {
                match combo {
                    "label" => key.label = value.ch.unwrap_or(0),
                    "number" => key.number = value.ch.unwrap_or(0),
                    _ => {
                        let mut mask = 0;
                        for token in combo.split('+') {
                            mask |= modifier_bit(token.trim());
                        }
                        key.variations.push(Variation {
                            meta: mask,
                            ch: value.ch.unwrap_or(0),
                            fallback: value.fallback,
                            is_none: value.is_none,
                        });
                    }
                }
            }
        }
        kcm
    }

    /// The character produced by `key_code` under `meta_state`.
    #[must_use]
    pub fn character(&self, key_code: i32, meta_state: i32) -> u16 {
        match self.best_variation(key_code, meta_state) {
            Some(v) if !v.is_none => v.ch,
            _ => 0,
        }
    }

    /// The display label for `key_code` (the label field, else the base char).
    #[must_use]
    pub fn display_label(&self, key_code: i32) -> u16 {
        let key = self.keys.get(&key_code);
        if let Some(k) = key {
            if k.label != 0 {
                return k.label;
            }
        }
        self.character(key_code, 0)
    }

    /// The number (dialpad) character for `key_code`.
    #[must_use]
    pub fn number(&self, key_code: i32) -> u16 {
        if let Some(k) = self.keys.get(&key_code) {
            if k.number != 0 {
                return k.number;
            }
            if (b'0' as u16..=b'9' as u16).contains(&k.label) {
                return k.label;
            }
        }
        0
    }

    /// The `fallback` action (`keyCode`, `metaState`) for `key_code`.
    #[must_use]
    pub fn fallback(&self, key_code: i32, meta_state: i32) -> Option<(i32, i32)> {
        let v = self.best_variation(key_code, meta_state)?;
        v.fallback.map(|code| (code, v.meta))
    }

    /// The character from `key_code` that appears in `chars` under `meta_state`.
    #[must_use]
    pub fn match_char(&self, key_code: i32, chars: &[u16], meta_state: i32) -> u16 {
        let Some(key) = self.keys.get(&key_code) else {
            return 0;
        };
        let mut best: Option<(u32, u16)> = None;
        for v in &key.variations {
            if v.is_none || v.ch == 0 || !chars.contains(&v.ch) {
                continue;
            }
            if (v.meta & meta_state) != v.meta {
                continue;
            }
            let bits = v.meta.count_ones();
            if best.is_none_or(|(b, _)| bits >= b) {
                best = Some((bits, v.ch));
            }
        }
        best.map(|(_, c)| c).unwrap_or(0)
    }

    /// Reverse lookup: a `(key_code, meta_state)` that produces `ch`.
    ///
    /// Prefers the fewest modifier bits, then the earliest variation.
    #[must_use]
    pub fn events_for(&self, ch: char) -> Option<(i32, i32)> {
        let want = ch as u32 as u16;
        let mut best: Option<(u32, i32, i32)> = None;
        for (&code, key) in &self.keys {
            for v in &key.variations {
                if v.is_none || v.ch != want || v.ch == 0 {
                    continue;
                }
                let bits = v.meta.count_ones();
                if best.is_none_or(|(b, _, _)| bits < b) {
                    best = Some((bits, code, v.meta));
                }
            }
        }
        best.map(|(_, code, meta)| (code, meta))
    }

    fn best_variation(&self, key_code: i32, meta_state: i32) -> Option<&Variation> {
        let key = self.keys.get(&key_code)?;
        let mut best: Option<(u32, &Variation)> = None;
        for v in &key.variations {
            if (v.meta & meta_state) != v.meta {
                continue;
            }
            let bits = v.meta.count_ones();
            if best.is_none_or(|(b, _)| bits > b) {
                best = Some((bits, v));
            }
        }
        best.map(|(_, v)| v)
    }
}

/// The AOSP-generic map ([`GENERIC_KCM`]).
#[must_use]
pub fn generic() -> Kcm {
    Kcm::parse(GENERIC_KCM)
}

struct ParsedValue {
    ch: Option<u16>,
    fallback: Option<i32>,
    is_none: bool,
}

fn parse_value(s: &str) -> ParsedValue {
    let mut out = ParsedValue {
        ch: None,
        fallback: None,
        is_none: false,
    };
    let mut rest = s.trim();
    if let Some(r) = rest.strip_prefix("none") {
        out.is_none = true;
        rest = r.trim();
    }
    if rest.starts_with('\'') {
        if let Some((c, r)) = parse_char_literal(rest) {
            out.ch = Some(c);
            rest = r.trim();
        }
    }
    if let Some(r) = rest.strip_prefix("fallback") {
        if let Some(code) = key_code_from_name(r.trim()) {
            out.fallback = Some(code);
        }
    }
    out
}

fn parse_char_literal(s: &str) -> Option<(u16, &str)> {
    let mut it = s.char_indices();
    if it.next()?.1 != '\'' {
        return None;
    }
    let (_, c) = it.next()?;
    let decoded = if c == '\\' {
        let (_, esc) = it.next()?;
        match esc {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '0' => '\0',
            '\\' => '\\',
            '\'' => '\'',
            'u' => {
                let mut hex = String::with_capacity(4);
                for _ in 0..4 {
                    hex.push(it.next()?.1);
                }
                char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?
            }
            other => other,
        }
    } else {
        c
    };
    let (idx, close) = it.next()?;
    if close != '\'' {
        return None;
    }
    Some((decoded as u32 as u16, &s[idx + 1..]))
}

fn modifier_bit(token: &str) -> i32 {
    match token {
        "shift" => META_SHIFT,
        "alt" => META_ALT,
        "ctrl" => META_CTRL,
        "meta" | "sym" => META_META | META_SYM,
        "capslock" => META_CAPS_LOCK,
        "numlock" => META_NUM_LOCK,
        _ => 0,
    }
}

fn strip_comment(line: &str) -> &str {
    let mut in_quote = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quote => escaped = true,
            '\'' => in_quote = !in_quote,
            '#' if !in_quote => return &line[..i],
            _ => {}
        }
    }
    line
}

pub fn key_code_from_name(name: &str) -> Option<i32> {
    Some(match name {
        "UNKNOWN" => 0,
        "SOFT_LEFT" => 1,
        "SOFT_RIGHT" => 2,
        "HOME" => 3,
        "BACK" => 4,
        "CALL" => 5,
        "ENDCALL" => 6,
        "0" => 7,
        "1" => 8,
        "2" => 9,
        "3" => 10,
        "4" => 11,
        "5" => 12,
        "6" => 13,
        "7" => 14,
        "8" => 15,
        "9" => 16,
        "STAR" => 17,
        "POUND" => 18,
        "DPAD_UP" => 19,
        "DPAD_DOWN" => 20,
        "DPAD_LEFT" => 21,
        "DPAD_RIGHT" => 22,
        "DPAD_CENTER" => 23,
        "VOLUME_UP" => 24,
        "VOLUME_DOWN" => 25,
        "POWER" => 26,
        "CAMERA" => 27,
        "CLEAR" => 28,
        "A" => 29,
        "B" => 30,
        "C" => 31,
        "D" => 32,
        "E" => 33,
        "F" => 34,
        "G" => 35,
        "H" => 36,
        "I" => 37,
        "J" => 38,
        "K" => 39,
        "L" => 40,
        "M" => 41,
        "N" => 42,
        "O" => 43,
        "P" => 44,
        "Q" => 45,
        "R" => 46,
        "S" => 47,
        "T" => 48,
        "U" => 49,
        "V" => 50,
        "W" => 51,
        "X" => 52,
        "Y" => 53,
        "Z" => 54,
        "COMMA" => 55,
        "PERIOD" => 56,
        "ALT_LEFT" => 57,
        "ALT_RIGHT" => 58,
        "SHIFT_LEFT" => 59,
        "SHIFT_RIGHT" => 60,
        "TAB" => 61,
        "SPACE" => 62,
        "SYM" => 63,
        "EXPLORER" => 64,
        "ENVELOPE" => 65,
        "ENTER" => 66,
        "DEL" => 67,
        "GRAVE" => 68,
        "MINUS" => 69,
        "EQUALS" => 70,
        "LEFT_BRACKET" => 71,
        "RIGHT_BRACKET" => 72,
        "BACKSLASH" => 73,
        "SEMICOLON" => 74,
        "APOSTROPHE" => 75,
        "SLASH" => 76,
        "AT" => 77,
        "NUM" => 78,
        "HEADSETHOOK" => 79,
        "FOCUS" => 80,
        "PLUS" => 81,
        "MENU" => 82,
        "NOTIFICATION" => 83,
        "SEARCH" => 84,
        "MEDIA_PLAY_PAUSE" => 85,
        "MEDIA_STOP" => 86,
        "MEDIA_NEXT" => 87,
        "MEDIA_PREVIOUS" => 88,
        "MEDIA_REWIND" => 89,
        "MEDIA_FAST_FORWARD" => 90,
        "MUTE" => 91,
        "PAGE_UP" => 92,
        "PAGE_DOWN" => 93,
        "PICTSYMBOLS" => 94,
        "SWITCH_CHARSET" => 95,
        "BUTTON_A" => 96,
        "BUTTON_B" => 97,
        "BUTTON_C" => 98,
        "BUTTON_X" => 99,
        "BUTTON_Y" => 100,
        "BUTTON_Z" => 101,
        "BUTTON_L1" => 102,
        "BUTTON_R1" => 103,
        "BUTTON_L2" => 104,
        "BUTTON_R2" => 105,
        "BUTTON_THUMBL" => 106,
        "BUTTON_THUMBR" => 107,
        "BUTTON_START" => 108,
        "BUTTON_SELECT" => 109,
        "BUTTON_MODE" => 110,
        "ESCAPE" => 111,
        "FORWARD_DEL" => 112,
        "CTRL_LEFT" => 113,
        "CTRL_RIGHT" => 114,
        "CAPS_LOCK" => 115,
        "SCROLL_LOCK" => 116,
        "META_LEFT" => 117,
        "META_RIGHT" => 118,
        "FUNCTION" => 119,
        "SYSRQ" => 120,
        "BREAK" => 121,
        "MOVE_HOME" => 122,
        "MOVE_END" => 123,
        "INSERT" => 124,
        "FORWARD" => 125,
        "MEDIA_PLAY" => 126,
        "MEDIA_PAUSE" => 127,
        "MEDIA_CLOSE" => 128,
        "MEDIA_EJECT" => 129,
        "MEDIA_RECORD" => 130,
        "F1" => 131,
        "F2" => 132,
        "F3" => 133,
        "F4" => 134,
        "F5" => 135,
        "F6" => 136,
        "F7" => 137,
        "F8" => 138,
        "F9" => 139,
        "F10" => 140,
        "F11" => 141,
        "F12" => 142,
        "NUM_LOCK" => 143,
        "NUMPAD_0" => 144,
        "NUMPAD_1" => 145,
        "NUMPAD_2" => 146,
        "NUMPAD_3" => 147,
        "NUMPAD_4" => 148,
        "NUMPAD_5" => 149,
        "NUMPAD_6" => 150,
        "NUMPAD_7" => 151,
        "NUMPAD_8" => 152,
        "NUMPAD_9" => 153,
        "NUMPAD_DIVIDE" => 154,
        "NUMPAD_MULTIPLY" => 155,
        "NUMPAD_SUBTRACT" => 156,
        "NUMPAD_ADD" => 157,
        "NUMPAD_DOT" => 158,
        "NUMPAD_COMMA" => 159,
        "NUMPAD_ENTER" => 160,
        "NUMPAD_EQUALS" => 161,
        "NUMPAD_LEFT_PAREN" => 162,
        "NUMPAD_RIGHT_PAREN" => 163,
        "VOLUME_MUTE" => 164,
        "INFO" => 165,
        "CHANNEL_UP" => 166,
        "CHANNEL_DOWN" => 167,
        "ZOOM_IN" => 168,
        "ZOOM_OUT" => 169,
        "TV" => 170,
        "WINDOW" => 171,
        "GUIDE" => 172,
        "DVR" => 173,
        "BOOKMARK" => 174,
        "CAPTIONS" => 175,
        "SETTINGS" => 176,
        "TV_POWER" => 177,
        "TV_INPUT" => 178,
        "STB_POWER" => 179,
        "STB_INPUT" => 180,
        "AVR_POWER" => 181,
        "AVR_INPUT" => 182,
        "PROG_RED" => 183,
        "PROG_GREEN" => 184,
        "PROG_YELLOW" => 185,
        "PROG_BLUE" => 186,
        "APP_SWITCH" => 187,
        "BUTTON_1" => 188,
        "BUTTON_2" => 189,
        "BUTTON_3" => 190,
        "BUTTON_4" => 191,
        "BUTTON_5" => 192,
        "BUTTON_6" => 193,
        "BUTTON_7" => 194,
        "BUTTON_8" => 195,
        "BUTTON_9" => 196,
        "BUTTON_10" => 197,
        "BUTTON_11" => 198,
        "BUTTON_12" => 199,
        "BUTTON_13" => 200,
        "BUTTON_14" => 201,
        "BUTTON_15" => 202,
        "BUTTON_16" => 203,
        "LANGUAGE_SWITCH" => 204,
        "MANNER_MODE" => 205,
        "3D_MODE" => 206,
        "CONTACTS" => 207,
        "CALENDAR" => 208,
        "MUSIC" => 209,
        "CALCULATOR" => 210,
        "ZENKAKU_HANKAKU" => 211,
        "EISU" => 212,
        "MUHENKAN" => 213,
        "HENKAN" => 214,
        "KATAKANA_HIRAGANA" => 215,
        "YEN" => 216,
        "RO" => 217,
        "KANA" => 218,
        "ASSIST" => 219,
        "BRIGHTNESS_DOWN" => 220,
        "BRIGHTNESS_UP" => 221,
        "MEDIA_AUDIO_TRACK" => 222,
        "SLEEP" => 223,
        "WAKEUP" => 224,
        "PAIRING" => 225,
        "MEDIA_TOP_MENU" => 226,
        "11" => 227,
        "12" => 228,
        "LAST_CHANNEL" => 229,
        "TV_DATA_SERVICE" => 230,
        "VOICE_ASSIST" => 231,
        "TV_RADIO_SERVICE" => 232,
        "TV_TELETEXT" => 233,
        "TV_NUMBER_ENTRY" => 234,
        "TV_TERRESTRIAL_ANALOG" => 235,
        "TV_TERRESTRIAL_DIGITAL" => 236,
        "TV_SATELLITE" => 237,
        "TV_SATELLITE_BS" => 238,
        "TV_SATELLITE_CS" => 239,
        "TV_SATELLITE_SERVICE" => 240,
        "TV_NETWORK" => 241,
        "TV_ANTENNA_CABLE" => 242,
        "TV_INPUT_HDMI_1" => 243,
        "TV_INPUT_HDMI_2" => 244,
        "TV_INPUT_HDMI_3" => 245,
        "TV_INPUT_HDMI_4" => 246,
        "TV_INPUT_COMPOSITE_1" => 247,
        "TV_INPUT_COMPOSITE_2" => 248,
        "TV_INPUT_COMPONENT_1" => 249,
        "TV_INPUT_COMPONENT_2" => 250,
        "TV_INPUT_VGA_1" => 251,
        "TV_AUDIO_DESCRIPTION" => 252,
        "TV_AUDIO_DESCRIPTION_MIX_UP" => 253,
        "TV_AUDIO_DESCRIPTION_MIX_DOWN" => 254,
        "TV_ZOOM_MODE" => 255,
        "TV_CONTENTS_MENU" => 256,
        "TV_MEDIA_CONTEXT_MENU" => 257,
        "TV_TIMER_PROGRAMMING" => 258,
        "HELP" => 259,
        "NAVIGATE_PREVIOUS" => 260,
        "NAVIGATE_NEXT" => 261,
        "NAVIGATE_IN" => 262,
        "NAVIGATE_OUT" => 263,
        "STEM_PRIMARY" => 264,
        "STEM_1" => 265,
        "STEM_2" => 266,
        "STEM_3" => 267,
        "DPAD_UP_LEFT" => 268,
        "DPAD_DOWN_LEFT" => 269,
        "DPAD_UP_RIGHT" => 270,
        "DPAD_DOWN_RIGHT" => 271,
        "MEDIA_SKIP_FORWARD" => 272,
        "MEDIA_SKIP_BACKWARD" => 273,
        "MEDIA_STEP_FORWARD" => 274,
        "MEDIA_STEP_BACKWARD" => 275,
        "SOFT_SLEEP" => 276,
        "CUT" => 277,
        "COPY" => 278,
        "PASTE" => 279,
        "SYSTEM_NAVIGATION_UP" => 280,
        "SYSTEM_NAVIGATION_DOWN" => 281,
        "SYSTEM_NAVIGATION_LEFT" => 282,
        "SYSTEM_NAVIGATION_RIGHT" => 283,
        "ALL_APPS" => 284,
        "REFRESH" => 285,
        "THUMBS_UP" => 286,
        "THUMBS_DOWN" => 287,
        "PROFILE_SWITCH" => 288,
        "VIDEO_APP_1" => 289,
        "VIDEO_APP_2" => 290,
        "VIDEO_APP_3" => 291,
        "VIDEO_APP_4" => 292,
        "VIDEO_APP_5" => 293,
        "VIDEO_APP_6" => 294,
        "VIDEO_APP_7" => 295,
        "VIDEO_APP_8" => 296,
        "FEATURED_APP_1" => 297,
        "FEATURED_APP_2" => 298,
        "FEATURED_APP_3" => 299,
        "FEATURED_APP_4" => 300,
        "DEMO_APP_1" => 301,
        "DEMO_APP_2" => 302,
        "DEMO_APP_3" => 303,
        "DEMO_APP_4" => 304,
        "KEYBOARD_BACKLIGHT_DOWN" => 305,
        "KEYBOARD_BACKLIGHT_UP" => 306,
        "KEYBOARD_BACKLIGHT_TOGGLE" => 307,
        "STYLUS_BUTTON_PRIMARY" => 308,
        "STYLUS_BUTTON_SECONDARY" => 309,
        "STYLUS_BUTTON_TERTIARY" => 310,
        "STYLUS_BUTTON_TAIL" => 311,
        "RECENT_APPS" => 312,
        "MACRO_1" => 313,
        "MACRO_2" => 314,
        "MACRO_3" => 315,
        "MACRO_4" => 316,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Key-code names used by the layout resolve to the API-34 constants.
    #[test]
    fn key_names_map_to_android_keycodes() {
        assert_eq!(key_code_from_name("A"), Some(29));
        assert_eq!(key_code_from_name("Z"), Some(54));
        assert_eq!(key_code_from_name("0"), Some(7));
        assert_eq!(key_code_from_name("NUMPAD_0"), Some(144));
        assert_eq!(key_code_from_name("SPACE"), Some(62));
        assert_eq!(key_code_from_name("COMMA"), Some(55));
        assert_eq!(key_code_from_name("BUTTON_A"), Some(96));
        assert_eq!(key_code_from_name("NOT_A_KEY"), None);
    }

    /// The shipped generic layout parses to a `FULL` keyboard with the
    /// documented letter/digit/punctuation mappings.
    #[test]
    fn generic_layout_parses_and_maps_characters() {
        let k = generic();
        assert_eq!(k.keyboard_type, KEYBOARD_FULL);
        // Letters: base / shift / capslock.
        assert_eq!(k.character(29, 0), 'a' as u16);
        assert_eq!(k.character(29, META_SHIFT), 'A' as u16);
        assert_eq!(k.character(29, META_CAPS_LOCK), 'A' as u16);
        assert_eq!(
            k.character(29, META_SHIFT | META_CAPS_LOCK),
            'a' as u16
        );
        // Digits and their shifted symbols.
        assert_eq!(k.character(8, 0), '1' as u16);
        assert_eq!(k.character(8, META_SHIFT), '!' as u16);
        assert_eq!(k.character(55, META_SHIFT), '<' as u16); // COMMA
        assert_eq!(k.character(62, 0), ' ' as u16); // SPACE
        // A key that produces nothing under base.
        assert_eq!(k.character(111, 0), 0); // ESCAPE (only a fallback)
    }

    #[test]
    fn labels_numbers_and_fallbacks() {
        let k = generic();
        assert_eq!(k.display_label(29), 'A' as u16);
        assert_eq!(k.number(29), 0);
        assert_eq!(k.number(8), '1' as u16);
        // `key SPACE { alt, meta: fallback SEARCH; ctrl: fallback LANGUAGE_SWITCH }`
        assert_eq!(k.fallback(62, META_ALT), Some((84, META_ALT))); // SEARCH
        assert_eq!(k.fallback(62, META_CTRL), Some((204, META_CTRL))); // LANGUAGE_SWITCH
        assert_eq!(k.fallback(62, 0), None);
    }

    #[test]
    fn match_char_prefers_the_matching_variation() {
        let k = generic();
        assert_eq!(k.match_char(29, &['A' as u16], 0), 0);
        assert_eq!(k.match_char(29, &['A' as u16], META_SHIFT), 'A' as u16);
        assert_eq!(k.match_char(29, &['a' as u16], 0), 'a' as u16);
    }

    #[test]
    fn reverse_lookup_builds_key_events() {
        let k = generic();
        assert_eq!(k.events_for('a'), Some((29, 0)));
        // `A` is produced by both `shift` and `capslock`; the shortest mask wins.
        assert_eq!(k.events_for('A'), Some((29, META_SHIFT)));
        assert_eq!(k.events_for(' '), Some((62, 0)));
        assert_eq!(k.events_for('\u{2603}'), None);
    }
}
