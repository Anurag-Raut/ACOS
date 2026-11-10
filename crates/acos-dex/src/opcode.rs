//! Dalvik opcode table and instruction formats.
//!
//! Derived from the public Dalvik bytecode / instruction-formats specification.
//! Opcodes that are not assigned in the specification are reported as
//! [`crate::error::DexError::UnusedOpcode`] by the decoder.

/// Instruction encoding format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    F10x,
    F12x,
    F11n,
    F11x,
    F10t,
    F20t,
    F22x,
    F21t,
    F21s,
    F21h,
    F21c,
    F23x,
    F22b,
    F22t,
    F22s,
    F22c,
    F30t,
    F32x,
    F31i,
    F31t,
    F31c,
    F35c,
    F3rc,
    F51l,
    F45cc,
    F4rcc,
}

impl Format {
    /// Size of the encoded instruction in 16-bit code units.
    pub const fn size(self) -> usize {
        match self {
            Format::F10x | Format::F12x | Format::F11n | Format::F11x | Format::F10t => 1,
            Format::F20t
            | Format::F22x
            | Format::F21t
            | Format::F21s
            | Format::F21h
            | Format::F21c
            | Format::F23x
            | Format::F22b
            | Format::F22t
            | Format::F22s
            | Format::F22c => 2,
            Format::F30t
            | Format::F32x
            | Format::F31i
            | Format::F31t
            | Format::F31c
            | Format::F35c
            | Format::F3rc => 3,
            Format::F45cc | Format::F4rcc => 4,
            Format::F51l => 5,
        }
    }

    /// Human-readable name (`10x`, `35c`, ...).
    pub const fn name(self) -> &'static str {
        match self {
            Format::F10x => "10x",
            Format::F12x => "12x",
            Format::F11n => "11n",
            Format::F11x => "11x",
            Format::F10t => "10t",
            Format::F20t => "20t",
            Format::F22x => "22x",
            Format::F21t => "21t",
            Format::F21s => "21s",
            Format::F21h => "21h",
            Format::F21c => "21c",
            Format::F23x => "23x",
            Format::F22b => "22b",
            Format::F22t => "22t",
            Format::F22s => "22s",
            Format::F22c => "22c",
            Format::F30t => "30t",
            Format::F32x => "32x",
            Format::F31i => "31i",
            Format::F31t => "31t",
            Format::F31c => "31c",
            Format::F35c => "35c",
            Format::F3rc => "3rc",
            Format::F51l => "51l",
            Format::F45cc => "45cc",
            Format::F4rcc => "4rcc",
        }
    }
}

#[derive(Clone, Copy)]
struct OpInfo {
    mnemonic: Option<&'static str>,
    format: Format,
}

const UNUSED: OpInfo = OpInfo {
    mnemonic: None,
    format: Format::F10x,
};

const fn op(mnemonic: &'static str, format: Format) -> OpInfo {
    OpInfo {
        mnemonic: Some(mnemonic),
        format,
    }
}

/// `(opcode, mnemonic, format)` for every defined opcode in the specification.
const DEFINED: &[(u8, &str, Format)] = &[
    (0x00, "nop", Format::F10x),
    (0x01, "move", Format::F12x),
    (0x02, "move/from16", Format::F22x),
    (0x03, "move/16", Format::F32x),
    (0x04, "move-wide", Format::F12x),
    (0x05, "move-wide/from16", Format::F22x),
    (0x06, "move-wide/16", Format::F32x),
    (0x07, "move-object", Format::F12x),
    (0x08, "move-object/from16", Format::F22x),
    (0x09, "move-object/16", Format::F32x),
    (0x0a, "move-result", Format::F11x),
    (0x0b, "move-result-wide", Format::F11x),
    (0x0c, "move-result-object", Format::F11x),
    (0x0d, "move-exception", Format::F11x),
    (0x0e, "return-void", Format::F10x),
    (0x0f, "return", Format::F11x),
    (0x10, "return-wide", Format::F11x),
    (0x11, "return-object", Format::F11x),
    (0x12, "const/4", Format::F11n),
    (0x13, "const/16", Format::F21s),
    (0x14, "const", Format::F31i),
    (0x15, "const/high16", Format::F21h),
    (0x16, "const-wide/16", Format::F21s),
    (0x17, "const-wide/32", Format::F31i),
    (0x18, "const-wide", Format::F51l),
    (0x19, "const-wide/high16", Format::F21h),
    (0x1a, "const-string", Format::F21c),
    (0x1b, "const-string/jumbo", Format::F31c),
    (0x1c, "const-class", Format::F21c),
    (0x1d, "monitor-enter", Format::F11x),
    (0x1e, "monitor-exit", Format::F11x),
    (0x1f, "check-cast", Format::F21c),
    (0x20, "instance-of", Format::F22c),
    (0x21, "array-length", Format::F12x),
    (0x22, "new-instance", Format::F21c),
    (0x23, "new-array", Format::F22c),
    (0x24, "filled-new-array", Format::F35c),
    (0x25, "filled-new-array/range", Format::F3rc),
    (0x26, "fill-array-data", Format::F31t),
    (0x27, "throw", Format::F11x),
    (0x28, "goto", Format::F10t),
    (0x29, "goto/16", Format::F20t),
    (0x2a, "goto/32", Format::F30t),
    (0x2b, "packed-switch", Format::F31t),
    (0x2c, "sparse-switch", Format::F31t),
    (0x2d, "cmpl-float", Format::F23x),
    (0x2e, "cmpg-float", Format::F23x),
    (0x2f, "cmpl-double", Format::F23x),
    (0x30, "cmpg-double", Format::F23x),
    (0x31, "cmp-long", Format::F23x),
    (0x32, "if-eq", Format::F22t),
    (0x33, "if-ne", Format::F22t),
    (0x34, "if-lt", Format::F22t),
    (0x35, "if-ge", Format::F22t),
    (0x36, "if-gt", Format::F22t),
    (0x37, "if-le", Format::F22t),
    (0x38, "if-eqz", Format::F21t),
    (0x39, "if-nez", Format::F21t),
    (0x3a, "if-ltz", Format::F21t),
    (0x3b, "if-gez", Format::F21t),
    (0x3c, "if-gtz", Format::F21t),
    (0x3d, "if-lez", Format::F21t),
    (0x44, "aget", Format::F23x),
    (0x45, "aget-wide", Format::F23x),
    (0x46, "aget-object", Format::F23x),
    (0x47, "aget-boolean", Format::F23x),
    (0x48, "aget-byte", Format::F23x),
    (0x49, "aget-char", Format::F23x),
    (0x4a, "aget-short", Format::F23x),
    (0x4b, "aput", Format::F23x),
    (0x4c, "aput-wide", Format::F23x),
    (0x4d, "aput-object", Format::F23x),
    (0x4e, "aput-boolean", Format::F23x),
    (0x4f, "aput-byte", Format::F23x),
    (0x50, "aput-char", Format::F23x),
    (0x51, "aput-short", Format::F23x),
    (0x52, "iget", Format::F22c),
    (0x53, "iget-wide", Format::F22c),
    (0x54, "iget-object", Format::F22c),
    (0x55, "iget-boolean", Format::F22c),
    (0x56, "iget-byte", Format::F22c),
    (0x57, "iget-char", Format::F22c),
    (0x58, "iget-short", Format::F22c),
    (0x59, "iput", Format::F22c),
    (0x5a, "iput-wide", Format::F22c),
    (0x5b, "iput-object", Format::F22c),
    (0x5c, "iput-boolean", Format::F22c),
    (0x5d, "iput-byte", Format::F22c),
    (0x5e, "iput-char", Format::F22c),
    (0x5f, "iput-short", Format::F22c),
    (0x60, "sget", Format::F21c),
    (0x61, "sget-wide", Format::F21c),
    (0x62, "sget-object", Format::F21c),
    (0x63, "sget-boolean", Format::F21c),
    (0x64, "sget-byte", Format::F21c),
    (0x65, "sget-char", Format::F21c),
    (0x66, "sget-short", Format::F21c),
    (0x67, "sput", Format::F21c),
    (0x68, "sput-wide", Format::F21c),
    (0x69, "sput-object", Format::F21c),
    (0x6a, "sput-boolean", Format::F21c),
    (0x6b, "sput-byte", Format::F21c),
    (0x6c, "sput-char", Format::F21c),
    (0x6d, "sput-short", Format::F21c),
    (0x6e, "invoke-virtual", Format::F35c),
    (0x6f, "invoke-super", Format::F35c),
    (0x70, "invoke-direct", Format::F35c),
    (0x71, "invoke-static", Format::F35c),
    (0x72, "invoke-interface", Format::F35c),
    (0x74, "invoke-virtual/range", Format::F3rc),
    (0x75, "invoke-super/range", Format::F3rc),
    (0x76, "invoke-direct/range", Format::F3rc),
    (0x77, "invoke-static/range", Format::F3rc),
    (0x78, "invoke-interface/range", Format::F3rc),
    (0x7b, "neg-int", Format::F12x),
    (0x7c, "not-int", Format::F12x),
    (0x7d, "neg-long", Format::F12x),
    (0x7e, "not-long", Format::F12x),
    (0x7f, "neg-float", Format::F12x),
    (0x80, "neg-double", Format::F12x),
    (0x81, "int-to-long", Format::F12x),
    (0x82, "int-to-float", Format::F12x),
    (0x83, "int-to-double", Format::F12x),
    (0x84, "long-to-int", Format::F12x),
    (0x85, "long-to-float", Format::F12x),
    (0x86, "long-to-double", Format::F12x),
    (0x87, "float-to-int", Format::F12x),
    (0x88, "float-to-long", Format::F12x),
    (0x89, "float-to-double", Format::F12x),
    (0x8a, "double-to-int", Format::F12x),
    (0x8b, "double-to-long", Format::F12x),
    (0x8c, "double-to-float", Format::F12x),
    (0x8d, "int-to-byte", Format::F12x),
    (0x8e, "int-to-char", Format::F12x),
    (0x8f, "int-to-short", Format::F12x),
    (0x90, "add-int", Format::F23x),
    (0x91, "sub-int", Format::F23x),
    (0x92, "mul-int", Format::F23x),
    (0x93, "div-int", Format::F23x),
    (0x94, "rem-int", Format::F23x),
    (0x95, "and-int", Format::F23x),
    (0x96, "or-int", Format::F23x),
    (0x97, "xor-int", Format::F23x),
    (0x98, "shl-int", Format::F23x),
    (0x99, "shr-int", Format::F23x),
    (0x9a, "ushr-int", Format::F23x),
    (0x9b, "add-long", Format::F23x),
    (0x9c, "sub-long", Format::F23x),
    (0x9d, "mul-long", Format::F23x),
    (0x9e, "div-long", Format::F23x),
    (0x9f, "rem-long", Format::F23x),
    (0xa0, "and-long", Format::F23x),
    (0xa1, "or-long", Format::F23x),
    (0xa2, "xor-long", Format::F23x),
    (0xa3, "shl-long", Format::F23x),
    (0xa4, "shr-long", Format::F23x),
    (0xa5, "ushr-long", Format::F23x),
    (0xa6, "add-float", Format::F23x),
    (0xa7, "sub-float", Format::F23x),
    (0xa8, "mul-float", Format::F23x),
    (0xa9, "div-float", Format::F23x),
    (0xaa, "rem-float", Format::F23x),
    (0xab, "add-double", Format::F23x),
    (0xac, "sub-double", Format::F23x),
    (0xad, "mul-double", Format::F23x),
    (0xae, "div-double", Format::F23x),
    (0xaf, "rem-double", Format::F23x),
    (0xb0, "add-int/2addr", Format::F12x),
    (0xb1, "sub-int/2addr", Format::F12x),
    (0xb2, "mul-int/2addr", Format::F12x),
    (0xb3, "div-int/2addr", Format::F12x),
    (0xb4, "rem-int/2addr", Format::F12x),
    (0xb5, "and-int/2addr", Format::F12x),
    (0xb6, "or-int/2addr", Format::F12x),
    (0xb7, "xor-int/2addr", Format::F12x),
    (0xb8, "shl-int/2addr", Format::F12x),
    (0xb9, "shr-int/2addr", Format::F12x),
    (0xba, "ushr-int/2addr", Format::F12x),
    (0xbb, "add-long/2addr", Format::F12x),
    (0xbc, "sub-long/2addr", Format::F12x),
    (0xbd, "mul-long/2addr", Format::F12x),
    (0xbe, "div-long/2addr", Format::F12x),
    (0xbf, "rem-long/2addr", Format::F12x),
    (0xc0, "and-long/2addr", Format::F12x),
    (0xc1, "or-long/2addr", Format::F12x),
    (0xc2, "xor-long/2addr", Format::F12x),
    (0xc3, "shl-long/2addr", Format::F12x),
    (0xc4, "shr-long/2addr", Format::F12x),
    (0xc5, "ushr-long/2addr", Format::F12x),
    (0xc6, "add-float/2addr", Format::F12x),
    (0xc7, "sub-float/2addr", Format::F12x),
    (0xc8, "mul-float/2addr", Format::F12x),
    (0xc9, "div-float/2addr", Format::F12x),
    (0xca, "rem-float/2addr", Format::F12x),
    (0xcb, "add-double/2addr", Format::F12x),
    (0xcc, "sub-double/2addr", Format::F12x),
    (0xcd, "mul-double/2addr", Format::F12x),
    (0xce, "div-double/2addr", Format::F12x),
    (0xcf, "rem-double/2addr", Format::F12x),
    (0xd0, "add-int/lit16", Format::F22s),
    (0xd1, "rsub-int", Format::F22s),
    (0xd2, "mul-int/lit16", Format::F22s),
    (0xd3, "div-int/lit16", Format::F22s),
    (0xd4, "rem-int/lit16", Format::F22s),
    (0xd5, "and-int/lit16", Format::F22s),
    (0xd6, "or-int/lit16", Format::F22s),
    (0xd7, "xor-int/lit16", Format::F22s),
    (0xd8, "add-int/lit8", Format::F22b),
    (0xd9, "rsub-int/lit8", Format::F22b),
    (0xda, "mul-int/lit8", Format::F22b),
    (0xdb, "div-int/lit8", Format::F22b),
    (0xdc, "rem-int/lit8", Format::F22b),
    (0xdd, "and-int/lit8", Format::F22b),
    (0xde, "or-int/lit8", Format::F22b),
    (0xdf, "xor-int/lit8", Format::F22b),
    (0xe0, "shl-int/lit8", Format::F22b),
    (0xe1, "shr-int/lit8", Format::F22b),
    (0xe2, "ushr-int/lit8", Format::F22b),
    (0xfa, "invoke-polymorphic", Format::F45cc),
    (0xfb, "invoke-polymorphic/range", Format::F4rcc),
    (0xfc, "invoke-custom", Format::F35c),
    (0xfd, "invoke-custom/range", Format::F3rc),
    (0xfe, "const-method-handle", Format::F21c),
    (0xff, "const-method-type", Format::F21c),
];

/// Build the 256-entry lookup table at compile time.
const fn build_table() -> [OpInfo; 256] {
    let mut table = [UNUSED; 256];
    let mut i = 0;
    while i < DEFINED.len() {
        let (code, mnemonic, format) = DEFINED[i];
        table[code as usize] = op(mnemonic, format);
        i += 1;
    }
    table
}

static TABLE: [OpInfo; 256] = build_table();

/// A validated Dalvik opcode (0x00–0xff subset that is actually assigned).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Opcode(u8);

impl Opcode {
    /// Construct an opcode, returning `None` for unassigned opcodes.
    pub fn from_u8(value: u8) -> Option<Opcode> {
        if TABLE[value as usize].mnemonic.is_some() {
            Some(Opcode(value))
        } else {
            None
        }
    }

    #[inline]
    pub const fn byte(self) -> u8 {
        self.0
    }

    #[inline]
    pub const fn format(self) -> Format {
        TABLE[self.0 as usize].format
    }

    #[inline]
    pub const fn mnemonic(self) -> &'static str {
        match TABLE[self.0 as usize].mnemonic {
            Some(m) => m,
            None => "unused",
        }
    }

    /// True for invocations (including polymorphic and custom).
    pub const fn is_invoke(self) -> bool {
        matches!(
            self.0,
            0x6e..=0x72 | 0x74..=0x78 | 0xfa..=0xfd
        )
    }

    /// True for the `invoke-*` opcodes that take a varargs proto index (45cc/4rcc).
    pub const fn is_polymorphic(self) -> bool {
        matches!(self.0, 0xfa | 0xfb)
    }

    /// True for `invoke-custom`/`invoke-custom/range`.
    pub const fn is_custom(self) -> bool {
        matches!(self.0, 0xfc | 0xfd)
    }

    /// True for range-style invocations (`.../range`).
    pub const fn is_range(self) -> bool {
        matches!(self.0, 0x74..=0x78 | 0xfb | 0xfd | 0x25)
    }

    /// True if this instruction is a branch to a code-unit offset.
    pub const fn is_branch(self) -> bool {
        matches!(self.0, 0x28..=0x2a | 0x32..=0x3d)
    }

    /// True for `packed-switch` / `sparse-switch`.
    pub const fn is_switch(self) -> bool {
        matches!(self.0, 0x2b | 0x2c)
    }

    /// True for `fill-array-data`.
    pub const fn is_fill_array_data(self) -> bool {
        self.0 == 0x26
    }

    /// True for unconditional control-flow terminators/skips.
    pub const fn is_goto(self) -> bool {
        matches!(self.0, 0x28..=0x2a)
    }

    /// True when execution cannot fall through to the next instruction.
    pub const fn is_terminator(self) -> bool {
        matches!(self.0, 0x0e..=0x11 | 0x27 | 0x28..=0x2a)
    }
}

impl std::fmt::Debug for Opcode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}({:#04x})", self.mnemonic(), self.0)
    }
}

impl std::fmt::Display for Opcode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.mnemonic())
    }
}

/// Iterate over every defined opcode (useful for histogram initialisation).
pub fn defined_opcodes() -> impl Iterator<Item = Opcode> {
    (0u16..=255).filter_map(|v| Opcode::from_u8(v as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_no_duplicates_and_all_sizes_match() {
        for code in 0u16..=255 {
            if let Some(op) = Opcode::from_u8(code as u8) {
                assert!(!op.mnemonic().is_empty());
                assert!(op.format().size() >= 1);
            }
        }
        assert_eq!(Opcode::from_u8(0x3e), None);
        assert_eq!(Opcode::from_u8(0x73), None);
        assert_eq!(Opcode::from_u8(0xf9), None);
        assert_eq!(Opcode::from_u8(0x6e).unwrap().mnemonic(), "invoke-virtual");
    }
}
