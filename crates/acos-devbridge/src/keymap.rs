use crate::pb::keyboard_event::{KeyCodeType, KeyEventType};
use crate::pb::KeyboardEvent;

const AMETA_SHIFT_ON: i32 = 0x0000_0001;
const AMETA_ALT_ON: i32 = 0x0000_0002;
const AMETA_ALT_LEFT_ON: i32 = 0x0000_0010;
const AMETA_ALT_RIGHT_ON: i32 = 0x0000_0020;
const AMETA_SHIFT_LEFT_ON: i32 = 0x0000_0040;
const AMETA_SHIFT_RIGHT_ON: i32 = 0x0000_0080;
const AMETA_CTRL_ON: i32 = 0x0000_1000;
const AMETA_CTRL_LEFT_ON: i32 = 0x0000_2000;
const AMETA_CTRL_RIGHT_ON: i32 = 0x0000_4000;
const AMETA_META_ON: i32 = 0x0001_0000;
const AMETA_META_LEFT_ON: i32 = 0x0002_0000;
const AMETA_META_RIGHT_ON: i32 = 0x0004_0000;

/// What `sendKey` translates to on the ACOS panel control path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslatedKey {
    Key {
        code: i32,
        event_type: &'static str,
        meta_state: i32,
    },
    Text(String),
}

/// Tracks active modifier keys sent by `expo-device-hub` (separate evdev
/// modifier `keydown`/`keyup` events) so subsequent keystrokes carry the
/// right Android `AMETA_*` state.
#[derive(Debug, Default)]
pub struct KeyboardState {
    shift_left: bool,
    shift_right: bool,
    ctrl_left: bool,
    ctrl_right: bool,
    alt_left: bool,
    alt_right: bool,
    meta_left: bool,
    meta_right: bool,
}

impl KeyboardState {
    #[must_use]
    pub fn meta_state(&self) -> i32 {
        let mut m = 0;
        if self.shift_left {
            m |= AMETA_SHIFT_ON | AMETA_SHIFT_LEFT_ON;
        }
        if self.shift_right {
            m |= AMETA_SHIFT_ON | AMETA_SHIFT_RIGHT_ON;
        }
        if self.ctrl_left {
            m |= AMETA_CTRL_ON | AMETA_CTRL_LEFT_ON;
        }
        if self.ctrl_right {
            m |= AMETA_CTRL_ON | AMETA_CTRL_RIGHT_ON;
        }
        if self.alt_left {
            m |= AMETA_ALT_ON | AMETA_ALT_LEFT_ON;
        }
        if self.alt_right {
            m |= AMETA_ALT_ON | AMETA_ALT_RIGHT_ON;
        }
        if self.meta_left {
            m |= AMETA_META_ON | AMETA_META_LEFT_ON;
        }
        if self.meta_right {
            m |= AMETA_META_ON | AMETA_META_RIGHT_ON;
        }
        m
    }

    pub fn translate(&mut self, ev: &KeyboardEvent) -> Result<TranslatedKey, String> {
        if !ev.text.is_empty() {
            return Ok(TranslatedKey::Text(ev.text.clone()));
        }
        let event_type = match KeyEventType::try_from(ev.event_type) {
            Ok(KeyEventType::Keydown) => "down",
            Ok(KeyEventType::Keyup) => "up",
            Ok(KeyEventType::Keypress) => "press",
            Err(_) => return Err(format!("unsupported KeyboardEvent eventType {}", ev.event_type)),
        };

        // Update modifier latch when an evdev modifier arrives.
        if ev.code_type == KeyCodeType::Evdev as i32 && ev.key_code > 0 {
            let pressed = event_type != "up";
            let mod_code = match ev.key_code {
                42 => {
                    self.shift_left = pressed;
                    Some(59) // KEYCODE_SHIFT_LEFT
                }
                54 => {
                    self.shift_right = pressed;
                    Some(60) // KEYCODE_SHIFT_RIGHT
                }
                29 => {
                    self.ctrl_left = pressed;
                    Some(113) // KEYCODE_CTRL_LEFT
                }
                97 => {
                    self.ctrl_right = pressed;
                    Some(114) // KEYCODE_CTRL_RIGHT
                }
                56 => {
                    self.alt_left = pressed;
                    Some(57) // KEYCODE_ALT_LEFT
                }
                100 => {
                    self.alt_right = pressed;
                    Some(58) // KEYCODE_ALT_RIGHT
                }
                125 => {
                    self.meta_left = pressed;
                    Some(117) // KEYCODE_META_LEFT
                }
                126 => {
                    self.meta_right = pressed;
                    Some(118) // KEYCODE_META_RIGHT
                }
                _ => None,
            };
            if let Some(code) = mod_code {
                return Ok(TranslatedKey::Key {
                    code,
                    event_type,
                    meta_state: self.meta_state(),
                });
            }
            let code = evdev_to_android(ev.key_code).ok_or_else(|| {
                format!("unsupported evdev keyCode {}", ev.key_code)
            })?;
            return Ok(TranslatedKey::Key {
                code,
                event_type,
                meta_state: self.meta_state(),
            });
        }

        if ev.key_code != 0 {
            return Err(format!(
                "unsupported KeyboardEvent codeType={} keyCode={}",
                ev.code_type, ev.key_code
            ));
        }

        if ev.key.is_empty() {
            return Err("KeyboardEvent missing text, key, and keyCode".to_string());
        }

        if matches!(ev.key.as_str(), "AppSwitch" | "Power") {
            return Err(format!("key '{}' is not supported in ACOS", ev.key));
        }

        if let Some((code, extra_meta)) = w3c_key_to_android(&ev.key) {
            return Ok(TranslatedKey::Key {
                code,
                event_type,
                meta_state: self.meta_state() | extra_meta,
            });
        }

        // Fallback for shifted printable ASCII symbols (e.g. "!", "?", "#") on keypress.
        if event_type == "press" && ev.key.chars().count() == 1 {
            let ch = ev.key.chars().next().unwrap();
            if ch.is_ascii() && !ch.is_ascii_control() {
                return Ok(TranslatedKey::Text(ev.key.clone()));
            }
        }

        Err(format!("unsupported KeyboardEvent key {:?}", ev.key))
    }
}

fn evdev_to_android(evdev: i32) -> Option<i32> {
    Some(match evdev {
        1 => 111,   // KEY_ESC -> KEYCODE_ESCAPE
        2..=10 => 8 + (evdev - 2), // KEY_1..KEY_9 -> KEYCODE_1..KEYCODE_9
        11 => 7,    // KEY_0 -> KEYCODE_0
        12 => 69,   // KEY_MINUS
        13 => 70,   // KEY_EQUAL
        14 => 67,   // KEY_BACKSPACE -> KEYCODE_DEL
        15 => 61,   // KEY_TAB
        16 => 45,   // Q
        17 => 51,   // W
        18 => 33,   // E
        19 => 46,   // R
        20 => 48,   // T
        21 => 53,   // Y
        22 => 49,   // U
        23 => 37,   // I
        24 => 43,   // O
        25 => 44,   // P
        26 => 71,   // LEFTBRACE
        27 => 72,   // RIGHTBRACE
        28 => 66,   // ENTER
        30 => 29,   // A
        31 => 47,   // S
        32 => 32,   // D
        33 => 34,   // F
        34 => 35,   // G
        35 => 36,   // H
        36 => 38,   // J
        37 => 39,   // K
        38 => 40,   // L
        39 => 74,   // SEMICOLON
        40 => 75,   // APOSTROPHE
        41 => 68,   // GRAVE
        43 => 73,   // BACKSLASH
        44 => 54,   // Z
        45 => 52,   // X
        46 => 31,   // C
        47 => 50,   // V
        48 => 30,   // B
        49 => 42,   // N
        50 => 41,   // M
        51 => 55,   // COMMA
        52 => 56,   // DOT
        53 => 76,   // SLASH
        57 => 62,   // SPACE
        102 => 122, // MOVE_HOME
        103 => 19,  // UP
        104 => 92,  // PAGE_UP
        105 => 21,  // LEFT
        106 => 22,  // RIGHT
        107 => 123, // MOVE_END
        108 => 20,  // DOWN
        109 => 93,  // PAGE_DOWN
        111 => 112, // DELETE -> FORWARD_DEL
        113 => 164, // MUTE -> VOLUME_MUTE
        114 => 25,  // VOLUMEDOWN
        115 => 24,  // VOLUMEUP
        158 => 4,   // BACK
        172 => 3,   // HOMEPAGE
        _ => return None,
    })
}

fn w3c_key_to_android(key: &str) -> Option<(i32, i32)> {
    let shift = AMETA_SHIFT_ON | AMETA_SHIFT_LEFT_ON;
    Some(match key {
        "GoBack" => (4, 0),
        "GoHome" => (3, 0),
        "Enter" => (66, 0),
        "Backspace" => (67, 0),
        "Tab" => (61, 0),
        "Escape" => (111, 0),
        "Delete" => (112, 0),
        "Home" => (122, 0),
        "End" => (123, 0),
        "PageUp" => (92, 0),
        "PageDown" => (93, 0),
        "ArrowUp" => (19, 0),
        "ArrowDown" => (20, 0),
        "ArrowLeft" => (21, 0),
        "ArrowRight" => (22, 0),
        "Spacebar" | " " => (62, 0),
        "," => (55, 0),
        "." => (56, 0),
        "`" => (68, 0),
        "-" => (69, 0),
        "=" => (70, 0),
        "[" => (71, 0),
        "]" => (72, 0),
        "\\" => (73, 0),
        ";" => (74, 0),
        "'" => (75, 0),
        "/" => (76, 0),
        "@" => (77, 0),
        "+" => (81, 0),
        s if s.len() == 1 => {
            let b = s.as_bytes()[0];
            match b {
                b'0'..=b'9' => (7 + i32::from(b - b'0'), 0),
                b'a'..=b'z' => (29 + i32::from(b - b'a'), 0),
                b'A'..=b'Z' => (29 + i32::from(b - b'A'), shift),
                _ => return None,
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_hub_evdev_and_w3c_and_text() {
        let mut ks = KeyboardState::default();
        let enter = KeyboardEvent {
            code_type: KeyCodeType::Evdev as i32,
            event_type: KeyEventType::Keypress as i32,
            key_code: 28,
            key: String::new(),
            text: String::new(),
        };
        assert_eq!(
            ks.translate(&enter).unwrap(),
            TranslatedKey::Key {
                code: 66,
                event_type: "press",
                meta_state: 0,
            }
        );

        let back = KeyboardEvent {
            code_type: 0,
            event_type: KeyEventType::Keypress as i32,
            key_code: 0,
            key: "GoBack".to_string(),
            text: String::new(),
        };
        assert_eq!(
            ks.translate(&back).unwrap(),
            TranslatedKey::Key {
                code: 4,
                event_type: "press",
                meta_state: 0,
            }
        );

        let txt = KeyboardEvent {
            code_type: 0,
            event_type: 0,
            key_code: 0,
            key: String::new(),
            text: "hello!".to_string(),
        };
        assert_eq!(
            ks.translate(&txt).unwrap(),
            TranslatedKey::Text("hello!".to_string())
        );

        let app_switch = KeyboardEvent {
            code_type: 0,
            event_type: KeyEventType::Keypress as i32,
            key_code: 0,
            key: "AppSwitch".to_string(),
            text: String::new(),
        };
        assert!(ks.translate(&app_switch).is_err());
    }
}
