//! `ResTable_config` parsing, the real Android resource-configuration
//! **matching** algorithm (`match` + `isBetterThan`), and `toString()`-style
//! labels used for differential testing against aapt.
//!
//! The matching algorithm is a port of AOSP
//! `frameworks/base/libs/androidfw/ResourceTypes.cpp`. The density preference
//! was calibrated against `aapt2 optimize --target-densities` on real resource
//! tables (see `tests/config_matching.rs`): among candidate densities the
//! engine picks the *smallest density that is not below the requested one*;
//! if every candidate is below the request it picks the largest. That is the
//! "scaling down is generally better than scaling up" rule.

use crate::res::bytes::{u16_at, u32_at};
use crate::res::error::Result;

// `ResTable_config` masks / constants (ResourceTypes.h).
/// `MASK_LAYOUTDIR`: layout direction lives in the top two bits of `screenLayout`.
pub const MASK_LAYOUTDIR: u8 = 0xC0;
/// `LAYOUTDIR_LTR`.
pub const LAYOUTDIR_LTR: u8 = 0x40;
/// `LAYOUTDIR_RTL`.
pub const LAYOUTDIR_RTL: u8 = 0x80;
/// `MASK_SCREENSIZE`.
pub const MASK_SCREENSIZE: u8 = 0x0F;
/// `SCREENSIZE_SMALL`.
pub const SCREENSIZE_SMALL: u8 = 0x01;
/// `SCREENSIZE_NORMAL`.
pub const SCREENSIZE_NORMAL: u8 = 0x02;
/// `SCREENSIZE_LARGE`.
pub const SCREENSIZE_LARGE: u8 = 0x03;
/// `SCREENSIZE_XLARGE`.
pub const SCREENSIZE_XLARGE: u8 = 0x04;
/// `MASK_SCREENLONG`.
pub const MASK_SCREENLONG: u8 = 0x30;
/// `SCREENLONG_NO`.
pub const SCREENLONG_NO: u8 = 0x10;
/// `SCREENLONG_YES`.
pub const SCREENLONG_YES: u8 = 0x20;
/// `MASK_UI_MODE_TYPE`.
pub const MASK_UI_MODE_TYPE: u8 = 0x0F;
/// `UI_MODE_TYPE_DESK`.
pub const UI_MODE_TYPE_DESK: u8 = 0x02;
/// `UI_MODE_TYPE_CAR`.
pub const UI_MODE_TYPE_CAR: u8 = 0x03;
/// `UI_MODE_TYPE_TELEVISION`.
pub const UI_MODE_TYPE_TELEVISION: u8 = 0x04;
/// `UI_MODE_TYPE_APPLIANCE`.
pub const UI_MODE_TYPE_APPLIANCE: u8 = 0x05;
/// `UI_MODE_TYPE_WATCH`.
pub const UI_MODE_TYPE_WATCH: u8 = 0x06;
/// `UI_MODE_TYPE_VR_HEADSET`.
pub const UI_MODE_TYPE_VR_HEADSET: u8 = 0x07;
/// `MASK_UI_MODE_NIGHT`.
pub const MASK_UI_MODE_NIGHT: u8 = 0x30;
/// `UI_MODE_NIGHT_NO`.
pub const UI_MODE_NIGHT_NO: u8 = 0x10;
/// `UI_MODE_NIGHT_YES`.
pub const UI_MODE_NIGHT_YES: u8 = 0x20;
/// `MASK_SCREENROUND`.
pub const MASK_SCREENROUND: u8 = 0x03;
/// `SCREENROUND_NO`.
pub const SCREENROUND_NO: u8 = 0x01;
/// `SCREENROUND_YES`.
pub const SCREENROUND_YES: u8 = 0x02;
/// `MASK_WIDE_COLOR_GAMUT`.
pub const MASK_WIDE_COLOR_GAMUT: u8 = 0x03;
/// `MASK_HDR`.
pub const MASK_HDR: u8 = 0x0C;

/// `DENSITY_DEFAULT`.
pub const DENSITY_DEFAULT: u16 = 0;
/// `DENSITY_LOW`.
pub const DENSITY_LOW: u16 = 120;
/// `DENSITY_MEDIUM`.
pub const DENSITY_MEDIUM: u16 = 160;
/// `DENSITY_TV`.
pub const DENSITY_TV: u16 = 213;
/// `DENSITY_HIGH`.
pub const DENSITY_HIGH: u16 = 240;
/// `DENSITY_XHIGH`.
pub const DENSITY_XHIGH: u16 = 320;
/// `DENSITY_XXHIGH`.
pub const DENSITY_XXHIGH: u16 = 480;
/// `DENSITY_XXXHIGH`.
pub const DENSITY_XXXHIGH: u16 = 640;
/// `DENSITY_ANY`.
pub const DENSITY_ANY: u16 = 0xFFFE;
/// `DENSITY_NONE`.
pub const DENSITY_NONE: u16 = 0xFFFF;

/// `ORIENTATION_PORT`.
pub const ORIENTATION_PORT: u8 = 0x01;
/// `ORIENTATION_LAND`.
pub const ORIENTATION_LAND: u8 = 0x02;
/// `ORIENTATION_SQUARE`.
pub const ORIENTATION_SQUARE: u8 = 0x03;

/// `TOUCHSCREEN_STYLUS`.
pub const TOUCHSCREEN_STYLUS: u8 = 0x02;
/// `TOUCHSCREEN_FINGER`.
pub const TOUCHSCREEN_FINGER: u8 = 0x03;

/// `KEYBOARD_NOKEYS`.
pub const KEYBOARD_NOKEYS: u8 = 0x01;
/// `KEYBOARD_QWERTY`.
pub const KEYBOARD_QWERTY: u8 = 0x02;
/// `KEYBOARD_12KEY`.
pub const KEYBOARD_12KEY: u8 = 0x03;

/// `NAVIGATION_NONAV`.
pub const NAVIGATION_NONAV: u8 = 0x01;
/// `NAVIGATION_DPAD`.
pub const NAVIGATION_DPAD: u8 = 0x02;
/// `NAVIGATION_TRACKBALL`.
pub const NAVIGATION_TRACKBALL: u8 = 0x03;
/// `NAVIGATION_WHEEL`.
pub const NAVIGATION_WHEEL: u8 = 0x04;

/// `MASK_KEYSHIDDEN`.
pub const MASK_KEYSHIDDEN: u8 = 0x03;
/// `MASK_NAVHIDDEN`.
pub const MASK_NAVHIDDEN: u8 = 0x0C;

/// A decoded `ResTable_config`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ResConfig {
    /// Declared size of the config struct (does not participate in matching).
    pub size: u32,
    /// Mobile country code.
    pub mcc: u16,
    /// Mobile network code.
    pub mnc: u16,
    /// Two-letter language code (ASCII, zero-padded).
    pub language: [u8; 2],
    /// Two-letter region code (ASCII, zero-padded).
    pub country: [u8; 2],
    /// Layout direction / screen size / screen long (`screenLayout`).
    pub screen_layout: u8,
    /// UI mode type / night mode (`uiMode`).
    pub ui_mode: u8,
    /// Smallest screen width in dp.
    pub smallest_screen_width_dp: u16,
    /// Screen width in dp.
    pub screen_width_dp: u16,
    /// Screen height in dp.
    pub screen_height_dp: u16,
    /// Screen orientation.
    pub orientation: u8,
    /// Touchscreen type.
    pub touchscreen: u8,
    /// Density bucket or literal dpi.
    pub density: u16,
    /// Keyboard type.
    pub keyboard: u8,
    /// Navigation type.
    pub navigation: u8,
    /// Input flags (`keysexposed`/`navhidden`/...).
    pub input_flags: u8,
    /// Physical screen width in px.
    pub screen_width: u16,
    /// Physical screen height in px.
    pub screen_height: u16,
    /// Platform SDK version (`vNN` qualifier).
    pub sdk_version: u16,
    /// Platform minor version.
    pub minor_version: u16,
    /// Locale script (`Latn`, ...), zero-padded.
    pub locale_script: [u8; 4],
    /// Locale variant, zero-padded.
    pub locale_variant: [u8; 8],
    /// Second screen config byte (`screenLayout2`: round).
    pub screen_layout2: u8,
    /// Color mode (wide gamut / HDR).
    pub color_mode: u8,
    /// Grammatical inflection (Android 14+).
    pub grammatical_inflection: u8,
}

impl ResConfig {
    /// Parse a `ResTable_config` whose `size` field begins at `at`.
    #[allow(clippy::int_plus_one)]
    pub fn parse(data: &[u8], at: usize) -> Result<ResConfig> {
        let size = u32_at(data, at).unwrap_or(0);
        let avail = data.len().saturating_sub(at);
        let n = (size as usize).min(avail);
        let mut c = ResConfig {
            size,
            ..Default::default()
        };
        // Each reader is guarded by the declared config size so a short config
        // (older platform) simply leaves later fields at zero.
        macro_rules! field {
            ($offset:expr, $width:expr, $body:expr) => {
                if n >= $offset + $width {
                    $body;
                }
            };
        }
        field!(4, 2, c.mcc = u16_at(data, at + 4)?);
        field!(6, 2, c.mnc = u16_at(data, at + 6)?);
        field!(8, 2, c.language = read_code(data, at + 8));
        field!(10, 2, c.country = read_code(data, at + 10));
        field!(12, 1, c.orientation = data[at + 12]);
        field!(13, 1, c.touchscreen = data[at + 13]);
        field!(14, 2, c.density = u16_at(data, at + 14)?);
        field!(16, 1, c.keyboard = data[at + 16]);
        field!(17, 1, c.navigation = data[at + 17]);
        field!(18, 1, c.input_flags = data[at + 18]);
        field!(20, 2, c.screen_width = u16_at(data, at + 20)?);
        field!(22, 2, c.screen_height = u16_at(data, at + 22)?);
        field!(24, 2, c.sdk_version = u16_at(data, at + 24)?);
        field!(26, 2, c.minor_version = u16_at(data, at + 26)?);
        field!(28, 1, c.screen_layout = data[at + 28]);
        field!(29, 1, c.ui_mode = data[at + 29]);
        field!(30, 2, c.smallest_screen_width_dp = u16_at(data, at + 30)?);
        field!(32, 2, c.screen_width_dp = u16_at(data, at + 32)?);
        field!(34, 2, c.screen_height_dp = u16_at(data, at + 34)?);
        field!(36, 4, c.locale_script = read_code4(data, at + 36));
        field!(40, 8, c.locale_variant = read_code8(data, at + 40));
        field!(48, 1, c.screen_layout2 = data[at + 48]);
        field!(49, 1, c.color_mode = data[at + 49]);
        field!(52, 1, c.grammatical_inflection = data[at + 52]);
        Ok(c)
    }

    /// True for the default configuration (no qualifiers at all).
    pub fn is_default(&self) -> bool {
        self.mcc == 0
            && self.mnc == 0
            && self.language == [0, 0]
            && self.country == [0, 0]
            && self.screen_layout == 0
            && self.ui_mode == 0
            && self.smallest_screen_width_dp == 0
            && self.screen_width_dp == 0
            && self.screen_height_dp == 0
            && self.orientation == 0
            && self.touchscreen == 0
            && self.density == 0
            && self.keyboard == 0
            && self.navigation == 0
            && self.input_flags == 0
            && self.screen_width == 0
            && self.screen_height == 0
            && self.sdk_version == 0
            && self.minor_version == 0
            && self.locale_script == [0; 4]
            && self.locale_variant == [0; 8]
            && self.screen_layout2 == 0
            && self.color_mode == 0
    }

    /// The layout direction component of `screenLayout`.
    pub fn layout_dir(&self) -> u8 {
        self.screen_layout & MASK_LAYOUTDIR
    }

    /// The screen-size component of `screenLayout`.
    pub fn screen_size(&self) -> u8 {
        self.screen_layout & MASK_SCREENSIZE
    }

    /// The UI-mode type component of `uiMode`.
    pub fn ui_mode_type(&self) -> u8 {
        self.ui_mode & MASK_UI_MODE_TYPE
    }

    /// The night-mode component of `uiMode`.
    pub fn ui_mode_night(&self) -> u8 {
        self.ui_mode & MASK_UI_MODE_NIGHT
    }

    /// Port of `ResTable_config::match` — a config is a *candidate* for
    /// `settings` when every qualifier it declares is compatible.
    pub fn matches(&self, settings: &ResConfig) -> bool {
        if self.mcc != 0 && self.mcc != settings.mcc {
            return false;
        }
        if self.mnc != 0 && self.mnc != settings.mnc {
            return false;
        }
        if self.language[0] != 0 && self.language != settings.language {
            return false;
        }
        if self.country[0] != 0 && self.country != settings.country {
            return false;
        }
        if self.locale_script[0] != 0 && self.locale_script != settings.locale_script {
            return false;
        }
        if self.locale_variant[0] != 0 && self.locale_variant != settings.locale_variant {
            return false;
        }
        if self.layout_dir() != 0 && self.layout_dir() != settings.layout_dir() {
            return false;
        }
        // A config that requires a larger screen than we have cannot match.
        if self.smallest_screen_width_dp != 0
            && self.smallest_screen_width_dp > settings.smallest_screen_width_dp
        {
            return false;
        }
        if self.screen_width_dp != 0 && self.screen_width_dp > settings.screen_width_dp {
            return false;
        }
        if self.screen_height_dp != 0 && self.screen_height_dp > settings.screen_height_dp {
            return false;
        }
        if self.screen_size() != 0 && self.screen_size() > settings.screen_size() {
            return false;
        }
        if (self.screen_layout & MASK_SCREENLONG) != 0
            && (self.screen_layout & MASK_SCREENLONG) != (settings.screen_layout & MASK_SCREENLONG)
        {
            return false;
        }
        if self.orientation != 0 && self.orientation != settings.orientation {
            return false;
        }
        if self.ui_mode_type() != 0 && self.ui_mode_type() != settings.ui_mode_type() {
            return false;
        }
        if self.ui_mode_night() != 0 && self.ui_mode_night() != settings.ui_mode_night() {
            return false;
        }
        if self.touchscreen != 0 && self.touchscreen != settings.touchscreen {
            return false;
        }
        if self.keyboard != 0 && self.keyboard != settings.keyboard {
            return false;
        }
        if self.navigation != 0 && self.navigation != settings.navigation {
            return false;
        }
        if (self.input_flags & MASK_KEYSHIDDEN) != 0
            && (self.input_flags & MASK_KEYSHIDDEN) != (settings.input_flags & MASK_KEYSHIDDEN)
        {
            return false;
        }
        if (self.input_flags & MASK_NAVHIDDEN) != 0
            && (self.input_flags & MASK_NAVHIDDEN) != (settings.input_flags & MASK_NAVHIDDEN)
        {
            return false;
        }
        if self.screen_layout2 != 0 && self.screen_layout2 != settings.screen_layout2 {
            return false;
        }
        if self.sdk_version != 0
            && settings.sdk_version != 0
            && self.sdk_version > settings.sdk_version
        {
            return false;
        }
        true
    }

    /// Port of `ResTable_config::isBetterThan` — is `self` a better match for
    /// `requested` than `other`? Only meaningful when both `matches`.
    pub fn is_better_than(&self, other: &ResConfig, requested: &ResConfig) -> bool {
        if (self.mcc != other.mcc) && requested.mcc != 0 {
            return self.mcc != 0;
        }
        if (self.mnc != other.mnc) && requested.mnc != 0 {
            return self.mnc != 0;
        }
        if (self.language != other.language) && requested.language[0] != 0 {
            return self.language[0] != 0;
        }
        if (self.country != other.country) && requested.country[0] != 0 {
            return self.country[0] != 0;
        }
        if (self.locale_script != other.locale_script) && requested.locale_script[0] != 0 {
            return self.locale_script[0] != 0;
        }
        if (self.locale_variant != other.locale_variant) && requested.locale_variant[0] != 0 {
            return self.locale_variant[0] != 0;
        }
        let self_ld = self.layout_dir();
        let other_ld = other.layout_dir();
        if self_ld != other_ld && requested.layout_dir() != 0 {
            return self_ld > other_ld;
        }
        if self.smallest_screen_width_dp != other.smallest_screen_width_dp
            && requested.smallest_screen_width_dp != 0
        {
            return self.smallest_screen_width_dp > other.smallest_screen_width_dp;
        }
        if requested.screen_width_dp != 0 && self.screen_width_dp != other.screen_width_dp {
            return self.screen_width_dp > other.screen_width_dp;
        }
        if requested.screen_height_dp != 0 && self.screen_height_dp != other.screen_height_dp {
            return self.screen_height_dp > other.screen_height_dp;
        }
        let self_ss = self.screen_size();
        let other_ss = other.screen_size();
        if self_ss != other_ss && requested.screen_size() != 0 {
            let req = requested.screen_size();
            let self_ge = self_ss >= req;
            let other_ge = other_ss >= req;
            match (self_ge, other_ge) {
                (true, true) => return self_ss < other_ss,
                (true, false) => return true,
                (false, true) => return false,
                (false, false) => return self_ss > other_ss,
            }
        }
        let self_long = self.screen_layout & MASK_SCREENLONG;
        let other_long = other.screen_layout & MASK_SCREENLONG;
        if self_long != other_long && (requested.screen_layout & MASK_SCREENLONG) != 0 {
            return self_long != 0;
        }
        let self_round = self.screen_layout2 & MASK_SCREENROUND;
        let other_round = other.screen_layout2 & MASK_SCREENROUND;
        if self_round != other_round && (requested.screen_layout2 & MASK_SCREENROUND) != 0 {
            return self_round != 0;
        }
        if self.orientation != other.orientation && requested.orientation != 0 {
            return self.orientation != 0;
        }
        if self.ui_mode_type() != other.ui_mode_type() && requested.ui_mode_type() != 0 {
            return self.ui_mode_type() != 0;
        }
        if self.ui_mode_night() != other.ui_mode_night() && requested.ui_mode_night() != 0 {
            return self.ui_mode_night() != 0;
        }
        if self.density != other.density {
            return density_is_better(self.density, other.density, requested.density);
        }
        if self.touchscreen != other.touchscreen && requested.touchscreen != 0 {
            return self.touchscreen != 0;
        }
        if self.keyboard != other.keyboard && requested.keyboard != 0 {
            return self.keyboard != 0;
        }
        if self.navigation != other.navigation && requested.navigation != 0 {
            return self.navigation != 0;
        }
        if (self.input_flags & MASK_KEYSHIDDEN) != (other.input_flags & MASK_KEYSHIDDEN)
            && (requested.input_flags & MASK_KEYSHIDDEN) != 0
        {
            return (self.input_flags & MASK_KEYSHIDDEN) != 0;
        }
        if (self.input_flags & MASK_NAVHIDDEN) != (other.input_flags & MASK_NAVHIDDEN)
            && (requested.input_flags & MASK_NAVHIDDEN) != 0
        {
            return (self.input_flags & MASK_NAVHIDDEN) != 0;
        }
        if self.sdk_version != other.sdk_version && requested.sdk_version != 0 {
            return self.sdk_version != 0;
        }
        false
    }

    /// `ResTable_config::toString()` — the aapt config label, e.g.
    /// `mcc310-mnc260-en-rUS-sw411dp-420dpi-port`.
    pub fn label(&self) -> String {
        if self.is_default() {
            return "(default)".to_string();
        }
        let mut parts: Vec<String> = Vec::new();
        if self.mcc != 0 {
            parts.push(format!("mcc{}", self.mcc));
        }
        if self.mnc != 0 {
            parts.push(format!("mnc{}", self.mnc));
        }
        let lang = code_to_string(&self.language);
        if !lang.is_empty() {
            let script = code_to_string(&self.locale_script);
            let variant = code_to_string(&self.locale_variant);
            let country = code_to_string(&self.country);
            if !script.is_empty() || !variant.is_empty() {
                let mut bcp = format!("b+{lang}");
                if !script.is_empty() {
                    bcp.push('+');
                    bcp.push_str(&script);
                }
                if !country.is_empty() {
                    bcp.push('+');
                    bcp.push_str(&country);
                }
                if !variant.is_empty() {
                    bcp.push('+');
                    bcp.push_str(&variant);
                }
                parts.push(bcp);
            } else {
                let mut loc = lang;
                if !country.is_empty() {
                    loc.push_str("-r");
                    loc.push_str(&country);
                }
                parts.push(loc);
            }
        }
        match self.layout_dir() {
            LAYOUTDIR_LTR => parts.push("ldltr".to_string()),
            LAYOUTDIR_RTL => parts.push("ldrtl".to_string()),
            _ => {}
        }
        if self.smallest_screen_width_dp != 0 {
            parts.push(format!("sw{}dp", self.smallest_screen_width_dp));
        }
        if self.screen_width_dp != 0 {
            parts.push(format!("w{}dp", self.screen_width_dp));
        }
        if self.screen_height_dp != 0 {
            parts.push(format!("h{}dp", self.screen_height_dp));
        }
        match self.screen_size() {
            SCREENSIZE_SMALL => parts.push("small".to_string()),
            SCREENSIZE_NORMAL => parts.push("normal".to_string()),
            SCREENSIZE_LARGE => parts.push("large".to_string()),
            SCREENSIZE_XLARGE => parts.push("xlarge".to_string()),
            _ => {}
        }
        match self.screen_layout & MASK_SCREENLONG {
            SCREENLONG_NO => parts.push("notlong".to_string()),
            SCREENLONG_YES => parts.push("long".to_string()),
            _ => {}
        }
        match self.screen_layout2 & MASK_SCREENROUND {
            SCREENROUND_NO => parts.push("notround".to_string()),
            SCREENROUND_YES => parts.push("round".to_string()),
            _ => {}
        }
        match self.orientation {
            ORIENTATION_PORT => parts.push("port".to_string()),
            ORIENTATION_LAND => parts.push("land".to_string()),
            ORIENTATION_SQUARE => parts.push("square".to_string()),
            _ => {}
        }
        match self.ui_mode_type() {
            UI_MODE_TYPE_DESK => parts.push("desk".to_string()),
            UI_MODE_TYPE_CAR => parts.push("car".to_string()),
            UI_MODE_TYPE_TELEVISION => parts.push("television".to_string()),
            UI_MODE_TYPE_APPLIANCE => parts.push("appliance".to_string()),
            UI_MODE_TYPE_WATCH => parts.push("watch".to_string()),
            UI_MODE_TYPE_VR_HEADSET => parts.push("vrheadset".to_string()),
            _ => {}
        }
        match self.ui_mode_night() {
            UI_MODE_NIGHT_YES => parts.push("night".to_string()),
            UI_MODE_NIGHT_NO => parts.push("notnight".to_string()),
            _ => {}
        }
        if self.density != 0 {
            parts.push(density_label(self.density));
        }
        match self.touchscreen {
            1 => parts.push("notouch".to_string()),
            TOUCHSCREEN_STYLUS => parts.push("stylus".to_string()),
            TOUCHSCREEN_FINGER => parts.push("finger".to_string()),
            _ => {}
        }
        match self.keyboard {
            KEYBOARD_NOKEYS => parts.push("nokeys".to_string()),
            KEYBOARD_QWERTY => parts.push("qwerty".to_string()),
            KEYBOARD_12KEY => parts.push("12key".to_string()),
            _ => {}
        }
        match self.navigation {
            NAVIGATION_NONAV => parts.push("nonav".to_string()),
            NAVIGATION_DPAD => parts.push("dpad".to_string()),
            NAVIGATION_TRACKBALL => parts.push("trackball".to_string()),
            NAVIGATION_WHEEL => parts.push("wheel".to_string()),
            _ => {}
        }
        match self.input_flags & MASK_KEYSHIDDEN {
            1 => parts.push("keysexposed".to_string()),
            2 => parts.push("keyshidden".to_string()),
            3 => parts.push("keyssoft".to_string()),
            _ => {}
        }
        match self.input_flags & MASK_NAVHIDDEN {
            4 => parts.push("navexposed".to_string()),
            8 => parts.push("navhidden".to_string()),
            _ => {}
        }
        if self.sdk_version != 0 {
            parts.push(format!("v{}", self.sdk_version));
        }
        if self.minor_version != 0 {
            parts.push(format!("v{}.{}", self.sdk_version, self.minor_version));
        }
        parts.join("-")
    }
}

fn read_code(data: &[u8], at: usize) -> [u8; 2] {
    let mut out = [0u8; 2];
    if let Some(s) = data.get(at..at + 2) {
        out.copy_from_slice(s);
    }
    out
}

fn read_code4(data: &[u8], at: usize) -> [u8; 4] {
    let mut out = [0u8; 4];
    if let Some(s) = data.get(at..at + 4) {
        out.copy_from_slice(s);
    }
    out
}

fn read_code8(data: &[u8], at: usize) -> [u8; 8] {
    let mut out = [0u8; 8];
    if let Some(s) = data.get(at..at + 8) {
        out.copy_from_slice(s);
    }
    out
}

fn code_to_string(code: &[u8]) -> String {
    code.iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as char)
        .collect()
}

fn density_label(density: u16) -> String {
    match density {
        DENSITY_ANY => "anydpi".to_string(),
        DENSITY_NONE => "nodpi".to_string(),
        DENSITY_LOW => "ldpi".to_string(),
        DENSITY_MEDIUM => "mdpi".to_string(),
        DENSITY_TV => "tvdpi".to_string(),
        DENSITY_HIGH => "hdpi".to_string(),
        DENSITY_XHIGH => "xhdpi".to_string(),
        DENSITY_XXHIGH => "xxhdpi".to_string(),
        DENSITY_XXXHIGH => "xxxhdpi".to_string(),
        other => format!("{other}dpi"),
    }
}

/// Density preference: smallest density not below `requested`, else largest.
///
/// Calibrated against `aapt2 optimize --target-densities` (see module docs).
pub fn density_is_better(this: u16, other: u16, requested: u16) -> bool {
    let this_d = if this == 0 { DENSITY_MEDIUM } else { this };
    let other_d = if other == 0 { DENSITY_MEDIUM } else { other };
    if this_d == DENSITY_ANY {
        return true;
    }
    if other_d == DENSITY_ANY {
        return false;
    }
    let req = if requested == 0 || requested == DENSITY_ANY {
        DENSITY_MEDIUM
    } else {
        requested
    };
    let this_ge = this_d >= req;
    let other_ge = other_d >= req;
    match (this_ge, other_ge) {
        (true, false) => true,
        (false, true) => false,
        (true, true) => this_d < other_d,
        (false, false) => this_d > other_d,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ResConfig {
        ResConfig::default()
    }

    #[test]
    fn dense_selection_rule_calibrated() {
        // mdpi(160) vs hdpi(240) for request 190 → hdpi (matches aapt2).
        assert!(density_is_better(DENSITY_HIGH, DENSITY_MEDIUM, 190));
        // request 160 → mdpi.
        assert!(density_is_better(DENSITY_MEDIUM, DENSITY_HIGH, 160));
        // request 161 → hdpi.
        assert!(density_is_better(DENSITY_HIGH, DENSITY_MEDIUM, 161));
        // mdpi vs xxxhdpi for 161 → xxxhdpi (strong downscale preference).
        assert!(density_is_better(DENSITY_XXXHIGH, DENSITY_MEDIUM, 161));
        // 420dpi request, xhdpi vs xxhdpi → xxhdpi.
        assert!(density_is_better(DENSITY_XXHIGH, DENSITY_XHIGH, 420));
        // Both above request: smaller wins.
        assert!(density_is_better(DENSITY_XXHIGH, DENSITY_XXXHIGH, 420));
    }

    #[test]
    fn locale_fallback_better_than() {
        let mut us = cfg();
        us.language = *b"en";
        us.country = *b"US";
        let mut en = cfg();
        en.language = *b"en";
        let def = cfg();
        let mut req = cfg();
        req.language = *b"en";
        req.country = *b"US";
        assert!(us.matches(&req));
        assert!(en.matches(&req));
        assert!(def.matches(&req));
        assert!(us.is_better_than(&en, &req));
        assert!(en.is_better_than(&def, &req));
        assert!(!en.is_better_than(&us, &req));
    }

    #[test]
    fn screen_size_larger_than_request_does_not_match() {
        let mut large = cfg();
        large.screen_layout = SCREENSIZE_LARGE;
        let mut normal = cfg();
        normal.screen_layout = SCREENSIZE_NORMAL;
        assert!(!large.matches(&normal));
        assert!(normal.matches(&normal));
    }

    #[test]
    fn smallest_width_must_fit() {
        let mut sw600 = cfg();
        sw600.smallest_screen_width_dp = 600;
        let mut sw411 = cfg();
        sw411.smallest_screen_width_dp = 411;
        assert!(!sw600.matches(&sw411));
        assert!(sw411.matches(&sw600));
    }

    #[test]
    fn label_matches_aapt_examples() {
        let mut c = cfg();
        c.mcc = 310;
        c.mnc = 260;
        c.language = *b"en";
        c.country = *b"US";
        c.density = DENSITY_XXHIGH;
        assert_eq!(c.label(), "mcc310-mnc260-en-rUS-xxhdpi");
        let mut b = cfg();
        b.language = *b"sr";
        b.locale_script = *b"Latn";
        assert_eq!(b.label(), "b+sr+Latn");
    }
}
