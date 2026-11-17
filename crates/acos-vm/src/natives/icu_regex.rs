//! ICU regex natives behind `java.util.regex`.
//!
//! On Android 14 the real `java.util.regex.Pattern`/`Matcher` bytecode is a thin
//! wrapper over `com.android.icu.util.regex.PatternNative` /
//! `MatcherNative`, which are JNI shims over ICU4C's `icu::RegexPattern`.
//! ACOS serves those shims from Rust so the *real* boot-classpath bytecode runs
//! unchanged.
//!
//! ## Engine choice and limits
//!
//! The `regex` crate cannot express backreferences or lookaround, so it cannot
//! back an ICU-compatible `java.util.regex`. Instead this module contains a
//! small, self-contained backtracking engine compiled to a linear instruction
//! list (with nested, atomic sub-programs for lookaround / atomic groups /
//! possessive quantifiers) and matched over **Unicode code points** derived
//! from the input's UTF-16 code units. Offsets reported to Java are always
//! UTF-16 code-unit indices, as `/data/local/tmp`'s `dalvikvm` / the framework
//! expects.
//!
//! Supported (verified against ART in `fixtures/expected-art/dexprogs`, see the
//! 386-case `26_regex_matrix` corpus): literals, `.` (with
//! `DOTALL`/`UNIX_LINES`), `\d \D \w \W \s \S \h \H \v \V`, character
//! classes/ranges/negation/intersection (`&&`), POSIX `[[:name:]]` and
//! `\p{...}` properties (all general categories, `sc=`/`Script=` scripts and the
//! binary properties below), full Unicode case folding for `CASE_INSENSITIVE`,
//! greedy/lazy/possessive quantifiers, `{n}`, `{n,}`, `{n,m}`, capturing /
//! non-capturing / named groups, `(?imsxdu)` inline flags, `LITERAL`,
//! alternation, backreferences (`\1`..`\n`, `\k<name>`), lookahead / lookbehind
//! (positive and negative, bounded variable length), atomic groups, `^ $ \A \z
//! \Z \b \B \G \R`, `\Q..\E`, hex / unicode / octal / control escapes, region
//! bounds, anchoring bounds and (look-ahead) transparent bounds.
//!
//! Constructs ICU accepts but this engine does not are reported as structured
//! `UNSUPPORTED_JAVA_API` (never silently mis-matched). Patterns that are
//! invalid *per ICU* raise a real `java.util.regex.PatternSyntaxException` with
//! a description, the pattern and an approximate 0-based index.
//!
//! Catastrophic backtracking is bounded by `ACOS_REGEX_MAX_STEPS` (default 20M
//! steps); exceeding it yields a structured `UNSUPPORTED_JAVA_API` instead of
//! spinning until the `HostGuard` wall clock fires.
//!
//! Known honest approximations (documented, not silent; see
//! `results/vm/REGEX_DIVERGENCES.md`):
//! * `\X` extended grapheme clusters and `\b{g}` are not implemented (`\b{g}`
//!   is rejected the way ART rejects it).
//! * Unicode *blocks* (`\p{blk=...}`, `\p{Block=...}`, `\p{In...}`) are not
//!   modelled (script-aligned `In<Script>` names fall back to scripts).
//! * Full case folding is applied to contiguous literal runs; folds that would
//!   have to span a quantifier boundary are not modelled.
//! * `hitEnd()`/`requireEnd()` follow ART's behaviour for end anchors and
//!   consuming instructions, but remain best-effort for lookaround.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use unicode_properties::general_category::{
    GeneralCategory, GeneralCategoryGroup, UnicodeGeneralCategory,
};
use unicode_script::{Script, UnicodeScript};

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, Handle, Object};
use crate::vm::{InvokeKind, Vm};

// ===========================================================================
// Compile-time flags (values shared with java.util.regex.Pattern / ICU)
// ===========================================================================

const F_UNIX_LINES: u32 = 0x01;
const F_CASE_INSENSITIVE: u32 = 0x02;
const F_COMMENTS: u32 = 0x04;
const F_MULTILINE: u32 = 0x08;
const F_LITERAL: u32 = 0x10;
const F_DOTALL: u32 = 0x20;
/// ICU is always Unicode-aware; `UNICODE_CASE` is accepted as a no-op.
const F_UNICODE_CASE: u32 = 0x40;

/// Hard ceiling on backtracking steps for a single match attempt. Reached only
/// by pathological patterns; the native returns a structured error instead of
/// spinning until the `HostGuard` wall clock fires.
const DEFAULT_MAX_MATCH_STEPS: u64 = 20_000_000;

fn max_match_steps() -> u64 {
    static LIMIT: OnceLock<u64> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var("ACOS_REGEX_MAX_STEPS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MAX_MATCH_STEPS)
    })
}

// ===========================================================================
// Instruction set
// ===========================================================================

#[derive(Clone, Debug)]
enum Inst {
    /// Match one code point (with optional simple case folding).
    Char(u32, bool),
    /// Match one code point against a character class.
    /// `u16` indexes `Pattern::classes`; `bool` is `CASE_INSENSITIVE`.
    Class(u16, bool),
    /// Match a case-insensitive literal run by full Unicode case folding.
    /// `usize` indexes `Pattern::folds`.
    FoldStr(u32),
    /// Match any code point honouring DOTALL / UNIX_LINES.
    Any { dotall: bool, unix: bool },
    /// Match a line break (`\R`): CRLF or a single line terminator.
    LineBreak,
    /// Try the first target, leaving the second as a backtrack point.
    Split(u32, u32),
    /// Unconditional jump.
    Jmp(u32),
    /// Record the current code-point position in capture slot `n`.
    Save(u16),
    /// Record the current position in hidden loop slot `n` (used to detect
    /// iterations that consume nothing so an unbounded quantifier cannot spin
    /// forever).
    SaveLoop(u16),
    /// End of an unbounded repetition: if the body consumed nothing this
    /// iteration, jump to `exit`, otherwise loop back to `start`.
    LoopEnd { start: u32, exit: u32, slot: u16 },
    /// Zero-width assertion.
    Assert(AssertKind),
    /// Backreference to capture slot `2n`.
    Backref(u16, bool),
    /// Positive/negative lookahead/lookbehind; runs an atomic sub-program.
    Look { prog: u32, behind: bool, neg: bool },
    /// Atomic group / possessive quantifier body (atomic sub-program).
    Atomic(u32),
    /// Overall success of this program.
    Match,
}

#[derive(Clone, Copy, Debug)]
enum AssertKind {
    StartInput,
    EndInput,
    EndInputOrFinalNewline,
    StartLine(bool),
    EndLine(bool),
    WordBoundary,
    NotWordBoundary,
    PrevMatchEnd,
}

#[derive(Clone, Debug)]
enum ClassNode {
    Empty,
    Range(u32, u32),
    Union(Vec<ClassNode>),
    Inter(Box<ClassNode>, Box<ClassNode>),
    Neg(Box<ClassNode>),
    Pred(Pred),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pred {
    Digit,
    Word,
    Space,
    Horiz,
    Vert,
    Ascii,
    Alnum,
    Punct,
    Graph,
    Print,
    Blank,
    Cntrl,
    XDigit,
    JavaLower,
    JavaUpper,
    JavaWhitespace,
    Cat(GeneralCategory),
    CatGroup(GeneralCategoryGroup),
    Script(Script),
    WhiteSpace,
    Alphabetic,
    Lowercase,
    Uppercase,
}

impl ClassNode {
    /// Case-aware membership test. Folding is applied *inside* negation so that
    /// `(?i)[^a]` rejects both `a` and `A`.
    fn contains(&self, c: u32, ic: bool) -> bool {
        match self {
            ClassNode::Empty => false,
            ClassNode::Range(a, b) => {
                if c >= *a && c <= *b {
                    return true;
                }
                if ic {
                    if let Some(ch) = char::from_u32(c) {
                        for v in ch.to_lowercase().chain(ch.to_uppercase()) {
                            let v = v as u32;
                            if v >= *a && v <= *b {
                                return true;
                            }
                        }
                        let mut folded = Vec::new();
                        fold_into(ch, &mut folded);
                        if folded.len() == 1 {
                            let f = folded[0] as u32;
                            if f >= *a && f <= *b {
                                return true;
                            }
                        }
                    }
                }
                false
            }
            ClassNode::Union(v) => v.iter().any(|n| n.contains(c, ic)),
            ClassNode::Inter(a, b) => a.contains(c, ic) && b.contains(c, ic),
            ClassNode::Neg(n) => !n.contains(c, ic),
            ClassNode::Pred(p) => {
                if pred_match(*p, c) {
                    return true;
                }
                if ic {
                    if let Some(ch) = char::from_u32(c) {
                        for v in ch.to_lowercase().chain(ch.to_uppercase()) {
                            if pred_match(*p, v as u32) {
                                return true;
                            }
                        }
                        let mut folded = Vec::new();
                        fold_into(ch, &mut folded);
                        if folded.len() == 1 && pred_match(*p, folded[0] as u32) {
                            return true;
                        }
                    }
                }
                false
            }
        }
    }
}

fn pred_match(p: Pred, c: u32) -> bool {
    let ch = match char::from_u32(c) {
        Some(ch) => ch,
        None => return false,
    };
    match p {
        Pred::Digit => ch.general_category() == GeneralCategory::DecimalNumber,
        Pred::Word => is_word_cp(c),
        Pred::Space | Pred::WhiteSpace => ch.is_whitespace(),
        Pred::Horiz => {
            matches!(
                c,
                0x09 | 0x20 | 0xA0 | 0x1680 | 0x180E | 0x202F | 0x205F | 0x3000
            ) || (0x2000..=0x200A).contains(&c)
        }
        Pred::Vert => matches!(c, 0x0A | 0x0B | 0x0C | 0x0D | 0x85 | 0x2028 | 0x2029),
        Pred::Ascii => c <= 0x7F,
        Pred::Alphabetic => ch.is_alphabetic(),
        Pred::JavaUpper | Pred::Uppercase => ch.is_uppercase(),
        Pred::JavaLower | Pred::Lowercase => ch.is_lowercase(),
        Pred::Alnum => ch.is_alphanumeric(),
        Pred::Punct => ch.general_category_group() == GeneralCategoryGroup::Punctuation,
        Pred::Graph => !ch.is_whitespace() && !ch.is_control(),
        Pred::Print => !ch.is_control(),
        Pred::Blank => c == 0x09 || ch.general_category() == GeneralCategory::SpaceSeparator,
        Pred::Cntrl => ch.general_category() == GeneralCategory::Control,
        Pred::XDigit => is_hex_digit(c),
        Pred::JavaWhitespace => java_whitespace(c),
        Pred::Cat(gc) => ch.general_category() == gc,
        Pred::CatGroup(g) => ch.general_category_group() == g,
        Pred::Script(s) => ch.script() == s,
    }
}

/// `Character.isWhitespace`: a space separator that is not non-breaking, or
/// one of the ASCII/control whitespace characters. Note this differs from the
/// Unicode `White_Space` property (`\s`) — notably NEL (U+0085) is *not*
/// whitespace for `Character.isWhitespace`, and NBSP (U+00A0) is not either.
fn java_whitespace(c: u32) -> bool {
    if matches!(
        c,
        0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x1C | 0x1D | 0x1E | 0x1F
    ) {
        return true;
    }
    match char::from_u32(c) {
        Some(ch) => {
            ch.general_category_group() == GeneralCategoryGroup::Separator
                && !matches!(c, 0xA0 | 0x2007 | 0x202F)
        }
        None => false,
    }
}

/// Unicode `Hex_Digit`: ASCII `0-9A-Fa-f` plus the fullwidth forms.
fn is_hex_digit(c: u32) -> bool {
    ch_ascii_hex(c)
        || (0xFF10..=0xFF19).contains(&c)
        || (0xFF21..=0xFF26).contains(&c)
        || (0xFF41..=0xFF46).contains(&c)
}

fn ch_ascii_hex(c: u32) -> bool {
    matches!(c, 0x30..=0x39 | 0x41..=0x46 | 0x61..=0x66)
}

fn is_word_cp(c: u32) -> bool {
    if matches!(c, 0x200C | 0x200D) {
        return true;
    }
    match char::from_u32(c) {
        Some(ch) => {
            ch.is_alphabetic()
                || ch.general_category() == GeneralCategory::DecimalNumber
                || ch.general_category_group() == GeneralCategoryGroup::Mark
                || ch.general_category() == GeneralCategory::ConnectorPunctuation
        }
        None => false,
    }
}

// ===========================================================================
// Compiled pattern
// ===========================================================================

struct Pattern {
    /// Program list; index 0 is the main program.
    programs: Vec<Vec<Inst>>,
    classes: Vec<ClassNode>,
    /// Full case-folded forms of case-insensitive literal runs, indexed by
    /// `Inst::FoldStr`.
    folds: Vec<Vec<char>>,
    group_count: usize,
    names: HashMap<String, usize>,
    /// Total capture slots (group slots + hidden loop slots).
    caps_len: usize,
    /// Index of the first hidden loop slot in the capture vector.
    caps_base: usize,
}

// ===========================================================================
// Parser
// ===========================================================================

#[derive(Debug)]
enum PErr {
    Syntax(String, usize),
    Unsupported(String, usize),
}

struct Parser {
    pat: Vec<char>,
    pos: usize,
    classes: Vec<ClassNode>,
    programs: Vec<Vec<Inst>>,
    folds: Vec<Vec<char>>,
    group_count: usize,
    names: HashMap<String, usize>,
    depth: usize,
    /// Number of hidden loop slots allocated so far.
    hidden_slots: usize,
    /// Non-zero while parsing the body of a look-behind group; unbounded
    /// quantifiers are rejected there because ICU requires a finite maximum
    /// length.
    lookbehind: usize,
}

const MAX_DEPTH: usize = 200;

impl Parser {
    fn new(pat: Vec<char>) -> Parser {
        Parser {
            pat,
            pos: 0,
            classes: Vec::new(),
            programs: vec![Vec::new()],
            folds: Vec::new(),
            group_count: 0,
            names: HashMap::new(),
            depth: 0,
            hidden_slots: 0,
            lookbehind: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.pat.get(self.pos).copied()
    }

    fn peek_at(&self, k: usize) -> Option<char> {
        self.pat.get(self.pos + k).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<(), PErr> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(PErr::Syntax(format!("Expected '{c}'"), self.pos))
        }
    }

    fn add_sub(&mut self, mut prog: Vec<Inst>) -> u32 {
        prog.push(Inst::Match);
        let idx = self.programs.len() as u32;
        self.programs.push(prog);
        idx
    }

    fn new_group(&mut self) -> usize {
        self.group_count += 1;
        self.group_count
    }

    fn push_fold(&mut self, folded: Vec<char>) -> u32 {
        self.folds.push(folded);
        (self.folds.len() - 1) as u32
    }

    fn new_hidden_slot(&mut self) -> u16 {
        let h = self.hidden_slots;
        self.hidden_slots += 1;
        h as u16
    }

    fn parse(mut self, initial_flags: u32) -> Result<Pattern, PErr> {
        let mut flags = initial_flags;
        let mut main;
        if flags & F_LITERAL != 0 {
            main = Vec::new();
            if flags & F_CASE_INSENSITIVE != 0 {
                let target = fold_string(&self.pat);
                let idx = self.push_fold(target);
                main.push(Inst::FoldStr(idx));
            } else {
                for &c in &self.pat {
                    main.push(Inst::Char(c as u32, false));
                }
            }
        } else {
            main = self.parse_alt(&mut flags)?;
            if self.pos < self.pat.len() {
                return Err(PErr::Syntax("Unmatched closing ')'".to_string(), self.pos));
            }
        }
        main.push(Inst::Match);
        self.programs[0] = main;
        let caps_base = (self.group_count + 1) * 2;
        let caps_len = caps_base + self.hidden_slots;
        Ok(Pattern {
            programs: self.programs,
            classes: self.classes,
            folds: self.folds,
            group_count: self.group_count,
            names: self.names,
            caps_len,
            caps_base,
        })
    }

    fn skip_comments(&mut self, flags: u32) {
        if flags & F_COMMENTS == 0 {
            return;
        }
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.pos += 1;
                }
                Some('#') => {
                    while let Some(c) = self.peek() {
                        self.pos += 1;
                        if c == '\n' {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
    }

    fn parse_alt(&mut self, flags: &mut u32) -> Result<Vec<Inst>, PErr> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(PErr::Unsupported(
                "pattern nesting too deep".to_string(),
                self.pos,
            ));
        }
        let first = self.parse_concat(flags)?;
        let mut alts = vec![first];
        while self.eat('|') {
            alts.push(self.parse_concat(flags)?);
        }
        self.depth -= 1;
        if alts.len() == 1 {
            return Ok(alts.pop().unwrap());
        }
        let n = alts.len();
        let mut buf: Vec<Inst> = Vec::new();
        let mut fixups: Vec<usize> = Vec::new();
        for (i, alt) in alts.into_iter().enumerate() {
            if i + 1 < n {
                let split_at = buf.len();
                buf.push(Inst::Split(0, 0));
                let a_start = buf.len() as u32;
                splice(&mut buf, alt);
                let jmp_at = buf.len();
                buf.push(Inst::Jmp(0));
                let next = buf.len() as u32;
                buf[split_at] = Inst::Split(a_start, next);
                fixups.push(jmp_at);
            } else {
                splice(&mut buf, alt);
            }
        }
        let end = buf.len() as u32;
        for f in fixups {
            buf[f] = Inst::Jmp(end);
        }
        Ok(buf)
    }

    fn parse_concat(&mut self, flags: &mut u32) -> Result<Vec<Inst>, PErr> {
        let mut buf: Vec<Inst> = Vec::new();
        let mut run: Vec<char> = Vec::new();
        loop {
            self.skip_comments(*flags);
            match self.peek() {
                None | Some('|') | Some(')') => break,
                _ => {
                    let atom = self.parse_quantified(flags)?;
                    // Coalesce runs of case-insensitive literals so that full
                    // case folding can match across character boundaries
                    // (e.g. pattern `ss` against input `\u00df`).
                    if !atom.is_empty() && atom.iter().all(|i| matches!(i, Inst::Char(_, true))) {
                        for inst in &atom {
                            if let Inst::Char(c, true) = inst {
                                run.push(char::from_u32(*c).unwrap_or('\u{FFFD}'));
                            }
                        }
                        continue;
                    }
                    flush_fold_run(&mut buf, &mut run, self);
                    splice(&mut buf, atom);
                }
            }
        }
        flush_fold_run(&mut buf, &mut run, self);
        Ok(buf)
    }

    fn parse_quantified(&mut self, flags: &mut u32) -> Result<Vec<Inst>, PErr> {
        let body = self.parse_atom(flags)?;
        match self.parse_quantifier()? {
            None => Ok(body),
            Some(q) => self.compile_repeat(body, q),
        }
    }

    fn parse_quantifier(&mut self) -> Result<Option<Quant>, PErr> {
        let (min, max) = match self.peek() {
            Some('*') => {
                self.pos += 1;
                (0, None)
            }
            Some('+') => {
                self.pos += 1;
                (1, None)
            }
            Some('?') => {
                self.pos += 1;
                (0, Some(1))
            }
            Some('{') => match self.try_brace()? {
                Some(v) => v,
                None => return Ok(None),
            },
            _ => return Ok(None),
        };
        let kind = match self.peek() {
            Some('?') => {
                self.pos += 1;
                QKind::Lazy
            }
            Some('+') => {
                self.pos += 1;
                QKind::Possessive
            }
            _ => QKind::Greedy,
        };
        if let Some(m) = max {
            if m < min {
                return Err(PErr::Syntax(
                    "Illegal repetition range".to_string(),
                    self.pos,
                ));
            }
        }
        if max.is_none() && self.lookbehind > 0 {
            return Err(PErr::Syntax(
                "Look-behind group does not have an obvious maximum length".to_string(),
                self.pos,
            ));
        }
        Ok(Some(Quant { min, max, kind }))
    }

    fn try_brace(&mut self) -> Result<Option<(u32, Option<u32>)>, PErr> {
        let save = self.pos;
        self.expect('{')?;
        let mut n1 = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                n1.push(c);
                self.pos += 1;
            } else {
                break;
            }
        }
        if n1.is_empty() {
            self.pos = save;
            return Ok(None);
        }
        let min: u32 = match n1.parse() {
            Ok(v) => v,
            Err(_) => {
                self.pos = save;
                return Ok(None);
            }
        };
        let max = if self.eat(',') {
            let mut n2 = String::new();
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    n2.push(c);
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if n2.is_empty() {
                None
            } else {
                match n2.parse() {
                    Ok(v) => Some(v),
                    Err(_) => {
                        self.pos = save;
                        return Ok(None);
                    }
                }
            }
        } else {
            Some(min)
        };
        if !self.eat('}') {
            self.pos = save;
            return Ok(None);
        }
        Ok(Some((min, max)))
    }

    fn compile_repeat(&mut self, body: Vec<Inst>, q: Quant) -> Result<Vec<Inst>, PErr> {
        let slot = if q.max.is_none() {
            self.new_hidden_slot()
        } else {
            0
        };
        if q.kind == QKind::Possessive {
            let mut sub: Vec<Inst> = Vec::new();
            emit_repeat(&mut sub, &body, q.min, q.max, false, slot)?;
            let idx = self.add_sub(sub);
            return Ok(vec![Inst::Atomic(idx)]);
        }
        let mut buf: Vec<Inst> = Vec::new();
        emit_repeat(&mut buf, &body, q.min, q.max, q.kind == QKind::Lazy, slot)?;
        Ok(buf)
    }

    fn parse_atom(&mut self, flags: &mut u32) -> Result<Vec<Inst>, PErr> {
        let c = match self.peek() {
            Some(c) => c,
            None => {
                return Err(PErr::Syntax(
                    "Unexpected end of pattern".to_string(),
                    self.pos,
                ))
            }
        };
        match c {
            '(' => self.parse_group(flags),
            '[' => {
                let node = self.parse_class(*flags)?;
                Ok(vec![Inst::Class(
                    self.push_class(node),
                    *flags & F_CASE_INSENSITIVE != 0,
                )])
            }
            '.' => {
                self.pos += 1;
                Ok(vec![Inst::Any {
                    dotall: *flags & F_DOTALL != 0,
                    unix: *flags & F_UNIX_LINES != 0,
                }])
            }
            '^' => {
                self.pos += 1;
                Ok(vec![Inst::Assert(AssertKind::StartLine(
                    *flags & F_MULTILINE != 0,
                ))])
            }
            '$' => {
                self.pos += 1;
                Ok(vec![Inst::Assert(AssertKind::EndLine(
                    *flags & F_MULTILINE != 0,
                ))])
            }
            '\\' => self.parse_escape(flags),
            ')' => Err(PErr::Syntax("Unmatched closing ')'".to_string(), self.pos)),
            '*' | '+' | '?' => Err(PErr::Syntax(format!("Dangling quantifier '{c}'"), self.pos)),
            _ => {
                self.pos += 1;
                Ok(vec![Inst::Char(c as u32, *flags & F_CASE_INSENSITIVE != 0)])
            }
        }
    }

    fn push_class(&mut self, node: ClassNode) -> u16 {
        self.classes.push(node);
        (self.classes.len() - 1) as u16
    }

    fn parse_group(&mut self, flags: &mut u32) -> Result<Vec<Inst>, PErr> {
        self.expect('(')?;
        if self.eat('?') {
            if self.eat(':') {
                let mut inner = *flags;
                let body = self.parse_alt(&mut inner)?;
                self.expect(')')?;
                return Ok(body);
            }
            if self.eat('=') {
                let mut inner = *flags;
                let body = self.parse_alt(&mut inner)?;
                self.expect(')')?;
                let sub = self.add_sub(body);
                return Ok(vec![Inst::Look {
                    prog: sub,
                    behind: false,
                    neg: false,
                }]);
            }
            if self.eat('!') {
                let mut inner = *flags;
                let body = self.parse_alt(&mut inner)?;
                self.expect(')')?;
                let sub = self.add_sub(body);
                return Ok(vec![Inst::Look {
                    prog: sub,
                    behind: false,
                    neg: true,
                }]);
            }
            if self.eat('>') {
                let mut inner = *flags;
                let body = self.parse_alt(&mut inner)?;
                self.expect(')')?;
                let sub = self.add_sub(body);
                return Ok(vec![Inst::Atomic(sub)]);
            }
            if self.eat('<') {
                if self.eat('=') {
                    let mut inner = *flags;
                    self.lookbehind += 1;
                    let body = self.parse_alt(&mut inner)?;
                    self.lookbehind -= 1;
                    self.expect(')')?;
                    let sub = self.add_sub(body);
                    return Ok(vec![Inst::Look {
                        prog: sub,
                        behind: true,
                        neg: false,
                    }]);
                }
                if self.eat('!') {
                    let mut inner = *flags;
                    self.lookbehind += 1;
                    let body = self.parse_alt(&mut inner)?;
                    self.lookbehind -= 1;
                    self.expect(')')?;
                    let sub = self.add_sub(body);
                    return Ok(vec![Inst::Look {
                        prog: sub,
                        behind: true,
                        neg: true,
                    }]);
                }
                let name = self.read_name('>')?;
                let idx = self.new_group();
                self.names.insert(name, idx);
                let mut inner = *flags;
                let body = self.parse_alt(&mut inner)?;
                self.expect(')')?;
                return Ok(wrap_capture(idx, body));
            }
            if self.eat('P') {
                if self.eat('<') {
                    let name = self.read_name('>')?;
                    let idx = self.new_group();
                    self.names.insert(name, idx);
                    let mut inner = *flags;
                    let body = self.parse_alt(&mut inner)?;
                    self.expect(')')?;
                    return Ok(wrap_capture(idx, body));
                }
                return Err(PErr::Unsupported(
                    "(?P<...>) named group syntax".to_string(),
                    self.pos,
                ));
            }
            // Inline flags: (?imsxdu) or (?imsxdu:...)
            self.parse_inline_flags(flags)
        } else {
            let idx = self.new_group();
            let mut inner = *flags;
            let body = self.parse_alt(&mut inner)?;
            self.expect(')')?;
            Ok(wrap_capture(idx, body))
        }
    }

    fn parse_inline_flags(&mut self, flags: &mut u32) -> Result<Vec<Inst>, PErr> {
        let mut add: u32 = 0;
        let mut rem: u32 = 0;
        let mut neg = false;
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => return Err(PErr::Syntax("Unclosed group".to_string(), self.pos)),
            };
            match c {
                '-' => {
                    self.pos += 1;
                    neg = true;
                }
                ':' => {
                    self.pos += 1;
                    let mut inner = (*flags | add) & !rem;
                    let body = self.parse_alt(&mut inner)?;
                    self.expect(')')?;
                    return Ok(body);
                }
                ')' => {
                    self.pos += 1;
                    if neg {
                        rem |= add;
                    }
                    *flags = (*flags | add) & !rem;
                    return Ok(Vec::new());
                }
                'i' | 'm' | 's' | 'x' | 'd' | 'u' => {
                    let bit = match c {
                        'i' => F_CASE_INSENSITIVE,
                        'm' => F_MULTILINE,
                        's' => F_DOTALL,
                        'x' => F_COMMENTS,
                        'd' => F_UNIX_LINES,
                        'u' => F_UNICODE_CASE,
                        _ => 0,
                    };
                    if neg {
                        rem |= bit;
                    } else {
                        add |= bit;
                    }
                    self.pos += 1;
                }
                'U' => {
                    // Android's Pattern parser does not accept the inline
                    // UNICODE_CHARACTER_CLASS flag (ART raises
                    // PatternSyntaxException).
                    return Err(PErr::Syntax(
                        "Unknown inline flag 'U'".to_string(),
                        self.pos,
                    ));
                }
                other => {
                    return Err(PErr::Syntax(
                        format!("Unknown inline flag '{other}'"),
                        self.pos,
                    ))
                }
            }
        }
    }

    fn read_name(&mut self, close: char) -> Result<String, PErr> {
        let mut name = String::new();
        loop {
            match self.bump() {
                Some(c) if c == close => break,
                Some(c) if c.is_ascii_alphanumeric() => name.push(c),
                _ => {
                    return Err(PErr::Syntax(
                        "Illegal capturing group name".to_string(),
                        self.pos,
                    ))
                }
            }
        }
        if name.is_empty()
            || name
                .chars()
                .next()
                .map(|c| !c.is_ascii_alphabetic())
                .unwrap_or(true)
        {
            return Err(PErr::Syntax(
                "Illegal capturing group name".to_string(),
                self.pos,
            ));
        }
        Ok(name)
    }

    // ---- escapes ---------------------------------------------------------

    fn parse_escape(&mut self, flags: &mut u32) -> Result<Vec<Inst>, PErr> {
        self.expect('\\')?;
        let c = match self.bump() {
            Some(c) => c,
            None => return Err(PErr::Syntax("Trailing backslash".to_string(), self.pos)),
        };
        let ic = *flags & F_CASE_INSENSITIVE != 0;
        let node = match c {
            'd' => Some(ClassNode::Pred(Pred::Digit)),
            'D' => Some(ClassNode::Neg(Box::new(ClassNode::Pred(Pred::Digit)))),
            'w' => Some(ClassNode::Pred(Pred::Word)),
            'W' => Some(ClassNode::Neg(Box::new(ClassNode::Pred(Pred::Word)))),
            's' => Some(ClassNode::Pred(Pred::Space)),
            'S' => Some(ClassNode::Neg(Box::new(ClassNode::Pred(Pred::Space)))),
            'h' => Some(ClassNode::Pred(Pred::Horiz)),
            'H' => Some(ClassNode::Neg(Box::new(ClassNode::Pred(Pred::Horiz)))),
            'v' => Some(ClassNode::Pred(Pred::Vert)),
            'V' => Some(ClassNode::Neg(Box::new(ClassNode::Pred(Pred::Vert)))),
            _ => None,
        };
        if let Some(n) = node {
            return Ok(vec![Inst::Class(
                self.push_class(n),
                *flags & F_CASE_INSENSITIVE != 0,
            )]);
        }
        match c {
            'b' => {
                // `\b{g}` (grapheme-cluster boundary) is not supported by
                // Android's Pattern; ART raises PatternSyntaxException.
                if self.peek() == Some('{')
                    && self.peek_at(1) == Some('g')
                    && self.peek_at(2) == Some('}')
                {
                    return Err(PErr::Syntax(
                        "Unsupported boundary matcher '\\b{g}'".to_string(),
                        self.pos,
                    ));
                }
                Ok(vec![Inst::Assert(AssertKind::WordBoundary)])
            }
            'B' => Ok(vec![Inst::Assert(AssertKind::NotWordBoundary)]),
            'A' => Ok(vec![Inst::Assert(AssertKind::StartInput)]),
            'z' => Ok(vec![Inst::Assert(AssertKind::EndInput)]),
            'Z' => Ok(vec![Inst::Assert(AssertKind::EndInputOrFinalNewline)]),
            'G' => Ok(vec![Inst::Assert(AssertKind::PrevMatchEnd)]),
            'R' => Ok(vec![Inst::LineBreak]),
            'p' | 'P' => {
                let mut n = self.parse_property_name()?;
                let mut negate = c == 'P';
                if n.starts_with('^') {
                    negate = !negate;
                    n.remove(0);
                }
                let mut node = resolve_property(&n).ok_or_else(|| {
                    PErr::Unsupported(format!("\\p{{{n}}} character property"), self.pos)
                })?;
                if negate {
                    node = ClassNode::Neg(Box::new(node));
                }
                Ok(vec![Inst::Class(
                    self.push_class(node),
                    *flags & F_CASE_INSENSITIVE != 0,
                )])
            }
            'k' => {
                if !self.eat('<') {
                    return Err(PErr::Syntax("Expected '<' after \\k".to_string(), self.pos));
                }
                let name = self.read_name('>')?;
                let idx = *self.names.get(&name).ok_or_else(|| {
                    PErr::Syntax(
                        format!("Named backreference to unknown group '{name}'"),
                        self.pos,
                    )
                })?;
                Ok(vec![Inst::Backref((idx * 2) as u16, ic)])
            }
            '1'..='9' => {
                let mut num = c.to_digit(10).unwrap() as usize;
                while let Some(d) = self.peek().and_then(|x| x.to_digit(10)) {
                    let cand = num * 10 + d as usize;
                    if cand > self.group_count {
                        break;
                    }
                    num = cand;
                    self.pos += 1;
                }
                if num > self.group_count {
                    return Err(PErr::Syntax(
                        format!("Backreference to non-existent group {num}"),
                        self.pos,
                    ));
                }
                Ok(vec![Inst::Backref((num * 2) as u16, ic)])
            }
            '0' => {
                let ch = self.parse_octal()?;
                Ok(vec![Inst::Char(ch, ic)])
            }
            'x' => {
                let ch = self.parse_hex_escape()?;
                Ok(vec![Inst::Char(ch, ic)])
            }
            'u' => {
                let ch = self.parse_unicode_escape()?;
                Ok(vec![Inst::Char(ch, ic)])
            }
            'c' => {
                let ch = self
                    .bump()
                    .ok_or_else(|| PErr::Syntax("Illegal control escape".to_string(), self.pos))?;
                if !ch.is_ascii_alphabetic() {
                    return Err(PErr::Syntax("Illegal control escape".to_string(), self.pos));
                }
                Ok(vec![Inst::Char((ch as u32) & 0x1F, ic)])
            }
            'Q' => {
                // Quoted literal run until \E (or end).
                let mut buf = Vec::new();
                loop {
                    if self.peek() == Some('\\') && self.peek_at(1) == Some('E') {
                        self.pos += 2;
                        break;
                    }
                    match self.bump() {
                        Some(ch) => buf.push(Inst::Char(ch as u32, ic)),
                        None => break,
                    }
                }
                Ok(buf)
            }
            'n' => Ok(vec![Inst::Char(0x0A, ic)]),
            'r' => Ok(vec![Inst::Char(0x0D, ic)]),
            't' => Ok(vec![Inst::Char(0x09, ic)]),
            'f' => Ok(vec![Inst::Char(0x0C, ic)]),
            'a' => Ok(vec![Inst::Char(0x07, ic)]),
            'e' => Ok(vec![Inst::Char(0x1B, ic)]),
            // ICU (without UREGEX_ERROR_ON_UNKNOWN_ESCAPES) treats an unknown
            // alphabetic escape as the literal character.
            other => Ok(vec![Inst::Char(other as u32, ic)]),
        }
    }

    fn parse_property_name(&mut self) -> Result<String, PErr> {
        if !self.eat('{') {
            // \pL shorthand (single-letter category)
            return match self.bump() {
                Some(c) => Ok(c.to_string()),
                None => Err(PErr::Syntax("Unclosed property".to_string(), self.pos)),
            };
        }
        let mut name = String::new();
        loop {
            match self.bump() {
                Some('}') => break,
                Some(c) => name.push(c),
                None => return Err(PErr::Syntax("Unclosed property name".to_string(), self.pos)),
            }
        }
        Ok(name)
    }

    fn parse_octal(&mut self) -> Result<u32, PErr> {
        // First digit after \0 has already been consumed by the caller via '0'.
        let mut val: u32 = 0;
        let mut count = 0;
        while count < 3 {
            match self.peek() {
                Some(d) if matches!(d, '0'..='7') => {
                    if count == 0 && d > '3' {
                        // only one more digit allowed after an initial >3
                        val = d.to_digit(8).unwrap();
                        self.pos += 1;
                        return Ok(val);
                    }
                    val = val * 8 + d.to_digit(8).unwrap();
                    self.pos += 1;
                    count += 1;
                }
                _ => break,
            }
        }
        if count == 0 {
            return Ok(0);
        }
        Ok(val)
    }

    fn parse_hex_escape(&mut self) -> Result<u32, PErr> {
        if self.eat('{') {
            let mut s = String::new();
            while let Some(c) = self.peek() {
                if c.is_ascii_hexdigit() {
                    s.push(c);
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if !self.eat('}') || s.is_empty() {
                return Err(PErr::Syntax(
                    "Unclosed hexadecimal escape sequence".to_string(),
                    self.pos,
                ));
            }
            let val = u32::from_str_radix(&s, 16).map_err(|_| {
                PErr::Syntax("Hexadecimal codepoint too large".to_string(), self.pos)
            })?;
            if val > 0x10FFFF {
                return Err(PErr::Syntax(
                    "Hexadecimal codepoint too large".to_string(),
                    self.pos,
                ));
            }
            return Ok(val);
        }
        let mut val = 0u32;
        for _ in 0..2 {
            let d = self.bump().ok_or_else(|| {
                PErr::Syntax("Illegal hexadecimal escape sequence".to_string(), self.pos)
            })?;
            val = val * 16
                + d.to_digit(16)
                    .ok_or_else(|| PErr::Syntax("Illegal hex digit".to_string(), self.pos))?;
        }
        Ok(val)
    }

    fn parse_unicode_escape(&mut self) -> Result<u32, PErr> {
        let mut val = 0u32;
        for _ in 0..4 {
            let d = self.bump().ok_or_else(|| {
                PErr::Syntax("Illegal Unicode escape sequence".to_string(), self.pos)
            })?;
            val = val * 16
                + d.to_digit(16)
                    .ok_or_else(|| PErr::Syntax("Illegal hex digit".to_string(), self.pos))?;
        }
        // Combine a following \uXXXX low surrogate.
        if (0xD800..=0xDBFF).contains(&val)
            && self.peek() == Some('\\')
            && self.peek_at(1) == Some('u')
        {
            let save = self.pos;
            self.pos += 2;
            let mut low = 0u32;
            let mut ok = true;
            for _ in 0..4 {
                match self.bump().and_then(|c| c.to_digit(16)) {
                    Some(d) => low = low * 16 + d,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok && (0xDC00..=0xDFFF).contains(&low) {
                return Ok(0x10000 + ((val - 0xD800) << 10) + (low - 0xDC00));
            }
            self.pos = save;
        }
        Ok(val)
    }

    // ---- character classes ----------------------------------------------

    fn parse_class(&mut self, flags: u32) -> Result<ClassNode, PErr> {
        self.expect('[')?;
        let neg = self.eat('^');
        let mut node = self.parse_class_body(flags)?;
        while self.peek() == Some('&') && self.peek_at(1) == Some('&') {
            self.pos += 2;
            let rhs = if self.peek() == Some('[') {
                self.parse_class(flags)?
            } else {
                self.parse_class_body(flags)?
            };
            node = ClassNode::Inter(Box::new(node), Box::new(rhs));
        }
        self.expect(']')?;
        if neg {
            node = ClassNode::Neg(Box::new(node));
        }
        Ok(node)
    }

    fn parse_class_body(&mut self, flags: u32) -> Result<ClassNode, PErr> {
        let mut items: Vec<ClassNode> = Vec::new();
        let mut first = true;
        loop {
            if self.peek().is_none() {
                return Err(PErr::Syntax(
                    "Unclosed character class".to_string(),
                    self.pos,
                ));
            }
            if self.peek() == Some(']') && !first {
                break;
            }
            if self.peek() == Some('&') && self.peek_at(1) == Some('&') {
                break;
            }
            let atom = self.parse_class_atom(flags)?;
            first = false;
            match atom {
                ClassAtom::Single(c) => {
                    if self.peek() == Some('-')
                        && self.peek_at(1).is_some()
                        && self.peek_at(1) != Some(']')
                    {
                        self.pos += 1;
                        match self.parse_class_atom(flags)? {
                            ClassAtom::Single(d) => {
                                if d < c {
                                    return Err(PErr::Syntax(
                                        "Illegal character range".to_string(),
                                        self.pos,
                                    ));
                                }
                                items.push(ClassNode::Range(c, d));
                            }
                            ClassAtom::Node(_) => {
                                return Err(PErr::Syntax(
                                    "Illegal character range".to_string(),
                                    self.pos,
                                ))
                            }
                        }
                    } else {
                        items.push(ClassNode::Range(c, c));
                    }
                }
                ClassAtom::Node(n) => items.push(n),
            }
        }
        Ok(match items.len() {
            0 => ClassNode::Empty,
            1 => items.pop().unwrap(),
            _ => ClassNode::Union(items),
        })
    }

    fn parse_class_atom(&mut self, flags: u32) -> Result<ClassAtom, PErr> {
        if self.peek() == Some('[') && self.peek_at(1) == Some(':') {
            return self.parse_posix_class();
        }
        match self.peek() {
            Some('[') => Ok(ClassAtom::Node(self.parse_class(flags)?)),
            Some('\\') => self.parse_class_escape(),
            Some(c) => {
                self.pos += 1;
                if c == ']' {
                    Ok(ClassAtom::Single(']' as u32))
                } else {
                    Ok(ClassAtom::Single(c as u32))
                }
            }
            None => Err(PErr::Syntax(
                "Unclosed character class".to_string(),
                self.pos,
            )),
        }
    }

    /// `[[:name:]]` POSIX bracket expression.
    fn parse_posix_class(&mut self) -> Result<ClassAtom, PErr> {
        self.expect('[')?;
        self.expect(':')?;
        let mut name = String::new();
        loop {
            match self.bump() {
                Some(':') => break,
                Some(c) if c.is_ascii_alphanumeric() => name.push(c),
                _ => {
                    return Err(PErr::Syntax(
                        "Unclosed POSIX character class".to_string(),
                        self.pos,
                    ))
                }
            }
        }
        self.expect(']')?;
        let pred = match name.to_ascii_lowercase().as_str() {
            "alpha" | "alphabetic" => Pred::Alphabetic,
            "digit" => Pred::Digit,
            "alnum" => Pred::Alnum,
            "punct" => Pred::Punct,
            "graph" => Pred::Graph,
            "print" => Pred::Print,
            "blank" => Pred::Blank,
            "cntrl" => Pred::Cntrl,
            "xdigit" => Pred::XDigit,
            "space" => Pred::WhiteSpace,
            "lower" => Pred::Lowercase,
            "upper" => Pred::Uppercase,
            "word" => Pred::Word,
            "ascii" => Pred::Ascii,
            _ => {
                return Err(PErr::Syntax(
                    format!("Unknown POSIX character class '{name}'"),
                    self.pos,
                ))
            }
        };
        Ok(ClassAtom::Node(ClassNode::Pred(pred)))
    }

    fn parse_class_escape(&mut self) -> Result<ClassAtom, PErr> {
        self.expect('\\')?;
        let c = match self.bump() {
            Some(c) => c,
            None => return Err(PErr::Syntax("Trailing backslash".to_string(), self.pos)),
        };
        let node = match c {
            'd' => Some(ClassNode::Pred(Pred::Digit)),
            'D' => Some(ClassNode::Neg(Box::new(ClassNode::Pred(Pred::Digit)))),
            'w' => Some(ClassNode::Pred(Pred::Word)),
            'W' => Some(ClassNode::Neg(Box::new(ClassNode::Pred(Pred::Word)))),
            's' => Some(ClassNode::Pred(Pred::Space)),
            'S' => Some(ClassNode::Neg(Box::new(ClassNode::Pred(Pred::Space)))),
            'h' => Some(ClassNode::Pred(Pred::Horiz)),
            'H' => Some(ClassNode::Neg(Box::new(ClassNode::Pred(Pred::Horiz)))),
            'v' => Some(ClassNode::Pred(Pred::Vert)),
            'V' => Some(ClassNode::Neg(Box::new(ClassNode::Pred(Pred::Vert)))),
            _ => None,
        };
        if let Some(n) = node {
            return Ok(ClassAtom::Node(n));
        }
        match c {
            'p' | 'P' => {
                let mut n = self.parse_property_name()?;
                let mut negate = c == 'P';
                if n.starts_with('^') {
                    negate = !negate;
                    n.remove(0);
                }
                let mut node = resolve_property(&n).ok_or_else(|| {
                    PErr::Unsupported(format!("\\p{{{n}}} character property"), self.pos)
                })?;
                if negate {
                    node = ClassNode::Neg(Box::new(node));
                }
                Ok(ClassAtom::Node(node))
            }
            'n' => Ok(ClassAtom::Single(0x0A)),
            'r' => Ok(ClassAtom::Single(0x0D)),
            't' => Ok(ClassAtom::Single(0x09)),
            'f' => Ok(ClassAtom::Single(0x0C)),
            'a' => Ok(ClassAtom::Single(0x07)),
            'e' => Ok(ClassAtom::Single(0x1B)),
            '0' => Ok(ClassAtom::Single(self.parse_octal()?)),
            'x' => Ok(ClassAtom::Single(self.parse_hex_escape()?)),
            'u' => Ok(ClassAtom::Single(self.parse_unicode_escape()?)),
            other => Ok(ClassAtom::Single(other as u32)),
        }
    }
}

enum ClassAtom {
    Single(u32),
    Node(ClassNode),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QKind {
    Greedy,
    Lazy,
    Possessive,
}

struct Quant {
    min: u32,
    max: Option<u32>,
    kind: QKind,
}

fn splice(buf: &mut Vec<Inst>, mut body: Vec<Inst>) {
    let base = buf.len() as u32;
    for inst in body.iter_mut() {
        offset_jumps(inst, base);
    }
    buf.extend(body);
}

fn offset_jumps(inst: &mut Inst, base: u32) {
    match inst {
        Inst::Split(a, b) => {
            *a += base;
            *b += base;
        }
        Inst::Jmp(a) => *a += base,
        Inst::LoopEnd { start, exit, .. } => {
            *start += base;
            *exit += base;
        }
        _ => {}
    }
}

// Full Unicode case-folding table (generated; see the file header).
include!("icu_regex_casefold.rs");

/// Applies full Unicode case folding to `c`, appending to `out`.
fn fold_into(c: char, out: &mut Vec<char>) {
    if !fold_special(c, out) {
        out.extend(c.to_lowercase());
    }
}

/// Full case fold of a literal run.
fn fold_string(chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    for &c in chars {
        fold_into(c, &mut out);
    }
    out
}

/// Emits the accumulated case-insensitive literal run as one `FoldStr`.
fn flush_fold_run(buf: &mut Vec<Inst>, run: &mut Vec<char>, parser: &mut Parser) {
    if run.is_empty() {
        return;
    }
    let target = fold_string(run);
    let idx = parser.push_fold(target);
    buf.push(Inst::FoldStr(idx));
    run.clear();
}

fn wrap_capture(idx: usize, mut body: Vec<Inst>) -> Vec<Inst> {
    for inst in body.iter_mut() {
        offset_jumps(inst, 1);
    }
    let mut buf = Vec::with_capacity(body.len() + 2);
    buf.push(Inst::Save((idx * 2) as u16));
    buf.extend(body);
    buf.push(Inst::Save((idx * 2 + 1) as u16));
    buf
}

fn emit_repeat(
    buf: &mut Vec<Inst>,
    body: &[Inst],
    min: u32,
    max: Option<u32>,
    lazy: bool,
    slot: u16,
) -> Result<(), PErr> {
    for _ in 0..min {
        splice(buf, body.to_vec());
    }
    match max {
        Some(m) => {
            let extra = m - min;
            for _ in 0..extra {
                let split_at = buf.len();
                buf.push(Inst::Split(0, 0));
                let bstart = buf.len() as u32;
                splice(buf, body.to_vec());
                let next = buf.len() as u32;
                buf[split_at] = if lazy {
                    Inst::Split(next, bstart)
                } else {
                    Inst::Split(bstart, next)
                };
            }
        }
        None => {
            let l1 = buf.len();
            buf.push(Inst::Split(0, 0));
            let bstart = buf.len() as u32;
            buf.push(Inst::SaveLoop(slot));
            splice(buf, body.to_vec());
            let loop_end = buf.len();
            buf.push(Inst::LoopEnd {
                start: l1 as u32,
                exit: 0,
                slot,
            });
            let lend = buf.len() as u32;
            if let Inst::LoopEnd { exit, .. } = &mut buf[loop_end] {
                *exit = lend;
            }
            buf[l1] = if lazy {
                Inst::Split(lend, bstart)
            } else {
                Inst::Split(bstart, lend)
            };
        }
    }
    Ok(())
}

/// Resolves a `\p{...}` property name to a class node, or `None` when the
/// property is not modelled (the caller then raises a structured
/// `UNSUPPORTED_JAVA_API`).
fn resolve_property(name: &str) -> Option<ClassNode> {
    let raw = name.trim();
    if raw.is_empty() {
        return None;
    }
    // `key=value` forms: gc=/General_Category=, sc=/Script=.
    if let Some((k, v)) = raw.split_once('=') {
        let key = k.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        let val = v.trim();
        return match key.as_str() {
            "gc" | "general_category" => resolve_general_category(val).map(ClassNode::Pred),
            "sc" | "script" => resolve_script(val).map(|s| ClassNode::Pred(Pred::Script(s))),
            // Blocks (`blk=`/`Block=`) are not modelled.
            _ => None,
        };
    }
    if let Some(n) = resolve_property_simple(raw) {
        return Some(n);
    }
    let lower = raw.to_ascii_lowercase();
    // `Is` prefix: binary property or script name.
    if lower.len() > 2 && lower.starts_with("is") {
        let rest = &raw[2..];
        if let Some(n) = resolve_property_simple(rest) {
            return Some(n);
        }
        if let Some(s) = resolve_script(rest) {
            return Some(ClassNode::Pred(Pred::Script(s)));
        }
    }
    // `In` prefix: block name. Only script-aligned blocks are approximated.
    if lower.len() > 2 && lower.starts_with("in") {
        let rest = &raw[2..];
        if let Some(s) = resolve_script(rest) {
            return Some(ClassNode::Pred(Pred::Script(s)));
        }
    }
    // Bare script name (e.g. `\p{Greek}`).
    resolve_script(raw).map(|s| ClassNode::Pred(Pred::Script(s)))
}

fn resolve_property_simple(name: &str) -> Option<ClassNode> {
    let norm = name.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    if let Some(gc) = resolve_general_category(&norm) {
        return Some(ClassNode::Pred(gc));
    }
    let pred = match norm.as_str() {
        "alphabetic" | "alpha" => Pred::Alphabetic,
        "white_space" | "whitespace" | "space" => Pred::WhiteSpace,
        "lowercase" | "lower" => Pred::Lowercase,
        "uppercase" | "upper" => Pred::Uppercase,
        "ascii" => Pred::Ascii,
        "alnum" => Pred::Alnum,
        "punct" | "punctuation" => Pred::Punct,
        "graph" => Pred::Graph,
        "print" => Pred::Print,
        "blank" => Pred::Blank,
        "cntrl" | "control" => Pred::Cntrl,
        "xdigit" => Pred::XDigit,
        "digit" => Pred::Digit,
        "word" => Pred::Word,
        "javalowercase" => Pred::JavaLower,
        "javauppercase" => Pred::JavaUpper,
        "javawhitespace" => Pred::JavaWhitespace,
        "javaalphabetic" => Pred::Alphabetic,
        "javadigit" => Pred::Cat(GeneralCategory::DecimalNumber),
        "javaletter" => Pred::CatGroup(GeneralCategoryGroup::Letter),
        "javatitlecase" => Pred::Cat(GeneralCategory::TitlecaseLetter),
        "javaletterordigit" => {
            return Some(ClassNode::Union(vec![
                ClassNode::Pred(Pred::CatGroup(GeneralCategoryGroup::Letter)),
                ClassNode::Pred(Pred::Cat(GeneralCategory::DecimalNumber)),
            ]))
        }
        _ => return None,
    };
    Some(ClassNode::Pred(pred))
}

fn resolve_general_category(name: &str) -> Option<Pred> {
    let n = name.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    let p = match n.as_str() {
        "l" | "letter" => Pred::CatGroup(GeneralCategoryGroup::Letter),
        "lu" | "uppercase_letter" => Pred::Cat(GeneralCategory::UppercaseLetter),
        "ll" | "lowercase_letter" => Pred::Cat(GeneralCategory::LowercaseLetter),
        "lt" | "titlecase_letter" => Pred::Cat(GeneralCategory::TitlecaseLetter),
        "lm" | "modifier_letter" => Pred::Cat(GeneralCategory::ModifierLetter),
        "lo" | "other_letter" => Pred::Cat(GeneralCategory::OtherLetter),
        "m" | "mark" => Pred::CatGroup(GeneralCategoryGroup::Mark),
        "mn" | "nonspacing_mark" => Pred::Cat(GeneralCategory::NonspacingMark),
        "mc" | "spacing_mark" => Pred::Cat(GeneralCategory::SpacingMark),
        "me" | "enclosing_mark" => Pred::Cat(GeneralCategory::EnclosingMark),
        "n" | "number" => Pred::CatGroup(GeneralCategoryGroup::Number),
        "nd" | "decimal_number" | "decimal_digit_number" => {
            Pred::Cat(GeneralCategory::DecimalNumber)
        }
        "nl" | "letter_number" => Pred::Cat(GeneralCategory::LetterNumber),
        "no" | "other_number" => Pred::Cat(GeneralCategory::OtherNumber),
        "p" | "punctuation" => Pred::CatGroup(GeneralCategoryGroup::Punctuation),
        "pc" | "connector_punctuation" => Pred::Cat(GeneralCategory::ConnectorPunctuation),
        "pd" | "dash_punctuation" => Pred::Cat(GeneralCategory::DashPunctuation),
        "ps" | "open_punctuation" => Pred::Cat(GeneralCategory::OpenPunctuation),
        "pe" | "close_punctuation" => Pred::Cat(GeneralCategory::ClosePunctuation),
        "pi" | "initial_punctuation" => Pred::Cat(GeneralCategory::InitialPunctuation),
        "pf" | "final_punctuation" => Pred::Cat(GeneralCategory::FinalPunctuation),
        "po" | "other_punctuation" => Pred::Cat(GeneralCategory::OtherPunctuation),
        "s" | "symbol" => Pred::CatGroup(GeneralCategoryGroup::Symbol),
        "sm" | "math_symbol" => Pred::Cat(GeneralCategory::MathSymbol),
        "sc" | "currency_symbol" => Pred::Cat(GeneralCategory::CurrencySymbol),
        "sk" | "modifier_symbol" => Pred::Cat(GeneralCategory::ModifierSymbol),
        "so" | "other_symbol" => Pred::Cat(GeneralCategory::OtherSymbol),
        "z" | "separator" => Pred::CatGroup(GeneralCategoryGroup::Separator),
        "zs" | "space_separator" => Pred::Cat(GeneralCategory::SpaceSeparator),
        "zl" | "line_separator" => Pred::Cat(GeneralCategory::LineSeparator),
        "zp" | "paragraph_separator" => Pred::Cat(GeneralCategory::ParagraphSeparator),
        "c" | "other" => Pred::CatGroup(GeneralCategoryGroup::Other),
        "cc" | "control" => Pred::Cat(GeneralCategory::Control),
        "cf" | "format" => Pred::Cat(GeneralCategory::Format),
        "cs" | "surrogate" => Pred::Cat(GeneralCategory::Surrogate),
        "co" | "private_use" => Pred::Cat(GeneralCategory::PrivateUse),
        "cn" | "unassigned" => Pred::Cat(GeneralCategory::Unassigned),
        _ => return None,
    };
    Some(p)
}

fn resolve_script(name: &str) -> Option<Script> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }
    if let Some(s) = Script::from_full_name(n) {
        return Some(s);
    }
    let spaced = n.replace(['_', '-'], " ");
    if let Some(s) = Script::from_full_name(&spaced) {
        return Some(s);
    }
    if let Some(s) = Script::from_short_name(n) {
        return Some(s);
    }
    // Common ICU short spellings.
    match n.to_ascii_lowercase().as_str() {
        "grek" => Some(Script::Greek),
        "latn" => Some(Script::Latin),
        "cyrl" => Some(Script::Cyrillic),
        "arab" => Some(Script::Arabic),
        "hani" => Some(Script::Han),
        "hebr" => Some(Script::Hebrew),
        "hira" => Some(Script::Hiragana),
        "kana" => Some(Script::Katakana),
        "hang" => Some(Script::Hangul),
        "deva" => Some(Script::Devanagari),
        _ => None,
    }
}

// ===========================================================================
// Input and matching
// ===========================================================================

struct Input {
    /// Original UTF-16 code units (kept so a matcher can be re-staged).
    units: Vec<u16>,
    cps: Vec<u32>,
    /// `cp_to_u16[k]` = UTF-16 index where code point `k` begins.
    cp_to_u16: Vec<usize>,
    /// `u16_to_cp[j]` = code-point index for UTF-16 index `j`.
    u16_to_cp: Vec<usize>,
}

impl Input {
    fn build(units: &[u16]) -> Input {
        let mut cps = Vec::with_capacity(units.len());
        let mut cp_to_u16 = Vec::with_capacity(units.len() + 1);
        let mut u16_to_cp = vec![0usize; units.len() + 1];
        cp_to_u16.push(0);
        let mut i = 0usize;
        let mut cp = 0usize;
        while i < units.len() {
            let u = units[i];
            let (c, w) = if (0xD800..=0xDBFF).contains(&u)
                && i + 1 < units.len()
                && (0xDC00..=0xDFFF).contains(&units[i + 1])
            {
                (
                    0x10000 + (((u as u32) - 0xD800) << 10) + ((units[i + 1] as u32) - 0xDC00),
                    2,
                )
            } else {
                (u as u32, 1)
            };
            cps.push(c);
            for k in 0..w {
                u16_to_cp[i + k] = cp;
            }
            i += w;
            cp += 1;
            cp_to_u16.push(i);
        }
        u16_to_cp[units.len()] = cp;
        Input {
            units: units.to_vec(),
            cps,
            cp_to_u16,
            u16_to_cp,
        }
    }
}

struct Ctx<'a> {
    input: &'a Input,
    region_start: usize,
    region_end: usize,
    anchoring: bool,
    transparent: bool,
    prev_match_end: usize,
    hit_end: Cell<bool>,
    require_end: Cell<bool>,
    /// Backtracking step counter shared across nested sub-programs.
    steps: Cell<u64>,
    /// Set when `steps` crosses `max_match_steps()`; the match aborts with a
    /// structured error instead of hanging.
    budget_hit: Cell<bool>,
}

impl Ctx<'_> {
    fn effective_start(&self) -> usize {
        if self.anchoring {
            self.region_start
        } else {
            0
        }
    }

    fn effective_end(&self) -> usize {
        if self.anchoring {
            self.region_end
        } else {
            self.input.cps.len()
        }
    }

    fn is_word_at(&self, cp: usize) -> bool {
        if !self.transparent && (cp < self.region_start || cp >= self.region_end) {
            return false;
        }
        self.input
            .cps
            .get(cp)
            .map(|&c| is_word_cp(c))
            .unwrap_or(false)
    }

    fn word_boundary(&self, cp: usize) -> bool {
        let before = if cp == 0 {
            false
        } else {
            self.is_word_at(cp - 1)
        };
        let after = self.is_word_at(cp);
        before != after
    }
}

fn is_line_terminator(c: u32) -> bool {
    matches!(c, 0x0A | 0x0B | 0x0C | 0x0D | 0x85 | 0x2028 | 0x2029)
}

fn is_final_newline(input: &Input, pos: usize, end: usize) -> bool {
    if pos >= end {
        return false;
    }
    let c = input.cps[pos];
    if c == 0x0D && pos + 1 < end && input.cps[pos + 1] == 0x0A {
        pos + 2 == end
    } else {
        is_line_terminator(c) && pos + 1 == end
    }
}

fn char_eq(a: u32, b: u32, ic: bool) -> bool {
    if a == b {
        return true;
    }
    if !ic {
        return false;
    }
    match (char::from_u32(a), char::from_u32(b)) {
        (Some(x), Some(y)) => {
            let mut fa = Vec::new();
            let mut fb = Vec::new();
            fold_into(x, &mut fa);
            fold_into(y, &mut fb);
            fa == fb
        }
        _ => false,
    }
}

fn class_contains(node: &ClassNode, c: u32, ic: bool) -> bool {
    node.contains(c, ic)
}

impl Pattern {
    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        prog_idx: u32,
        start: usize,
        limit: usize,
        caps_in: Vec<Option<usize>>,
        ctx: &Ctx,
        must_end: Option<usize>,
    ) -> Option<(usize, Vec<Option<usize>>)> {
        let prog = &self.programs[prog_idx as usize];
        let mut pc = 0usize;
        let mut pos = start;
        let mut caps = caps_in;
        let mut stack: Vec<(usize, usize, Vec<Option<usize>>)> = Vec::new();
        'main: loop {
            let step = ctx.steps.get() + 1;
            if step > max_match_steps() {
                ctx.budget_hit.set(true);
                return None;
            }
            ctx.steps.set(step);
            match &prog[pc] {
                Inst::Match => {
                    if must_end.is_none_or(|e| pos == e) {
                        return Some((pos, caps));
                    }
                }
                Inst::Char(c, ic) => {
                    if pos < limit {
                        if char_eq(ctx.input.cps[pos], *c, *ic) {
                            pos += 1;
                            pc += 1;
                            continue 'main;
                        }
                    } else {
                        ctx.hit_end.set(true);
                    }
                }
                Inst::Class(ci, ic) => {
                    if pos < limit {
                        let node = &self.classes[*ci as usize];
                        if class_contains(node, ctx.input.cps[pos], *ic) {
                            pos += 1;
                            pc += 1;
                            continue 'main;
                        }
                    } else {
                        ctx.hit_end.set(true);
                    }
                }
                Inst::FoldStr(fi) => {
                    let target = &self.folds[*fi as usize];
                    let max_k = target.len().min(limit.saturating_sub(pos));
                    let mut buf: Vec<char> = Vec::with_capacity(target.len());
                    let mut chosen: Option<usize> = None;
                    let mut alts: Vec<usize> = Vec::new();
                    for k in 0..=max_k {
                        buf.clear();
                        for &cp in &ctx.input.cps[pos..pos + k] {
                            match char::from_u32(cp) {
                                Some(ch) => fold_into(ch, &mut buf),
                                None => buf.push('\u{FFFD}'),
                            }
                        }
                        if buf == *target {
                            if chosen.is_none() {
                                chosen = Some(k);
                            } else {
                                alts.push(k);
                            }
                        }
                    }
                    if let Some(k) = chosen {
                        for a in alts {
                            stack.push((pc + 1, pos + a, caps.clone()));
                        }
                        pos += k;
                        pc += 1;
                        continue 'main;
                    }
                    ctx.hit_end.set(true);
                }
                Inst::Any { dotall, unix } => {
                    if pos < limit {
                        let c = ctx.input.cps[pos];
                        let line = if *unix {
                            c == 0x0A
                        } else {
                            is_line_terminator(c)
                        };
                        if *dotall || !line {
                            pos += 1;
                            pc += 1;
                            continue 'main;
                        }
                    } else {
                        ctx.hit_end.set(true);
                    }
                }
                Inst::LineBreak => {
                    if pos < limit {
                        let c = ctx.input.cps[pos];
                        if c == 0x0D && pos + 1 < limit && ctx.input.cps[pos + 1] == 0x0A {
                            pos += 2;
                            pc += 1;
                            continue 'main;
                        } else if is_line_terminator(c) {
                            pos += 1;
                            pc += 1;
                            continue 'main;
                        }
                    } else {
                        ctx.hit_end.set(true);
                    }
                }
                Inst::Split(a, b) => {
                    stack.push((*b as usize, pos, caps.clone()));
                    pc = *a as usize;
                    continue 'main;
                }
                Inst::Jmp(a) => {
                    pc = *a as usize;
                    continue 'main;
                }
                Inst::Save(n) => {
                    if (*n as usize) < caps.len() {
                        caps[*n as usize] = Some(pos);
                    }
                    pc += 1;
                    continue 'main;
                }
                Inst::SaveLoop(h) => {
                    let idx = self.caps_base + *h as usize;
                    if idx < caps.len() {
                        caps[idx] = Some(pos);
                    }
                    pc += 1;
                    continue 'main;
                }
                Inst::LoopEnd { start, exit, slot } => {
                    let idx = self.caps_base + *slot as usize;
                    let empty = caps.get(idx).copied().flatten() == Some(pos);
                    pc = if empty {
                        *exit as usize
                    } else {
                        *start as usize
                    };
                    continue 'main;
                }
                Inst::Backref(n, ic) => {
                    let s = caps.get(*n as usize).copied().flatten();
                    let e = caps.get(*n as usize + 1).copied().flatten();
                    if let (Some(s), Some(e)) = (s, e) {
                        let len = e - s;
                        if pos + len <= limit {
                            let mut ok = true;
                            for k in 0..len {
                                if !char_eq(ctx.input.cps[s + k], ctx.input.cps[pos + k], *ic) {
                                    ok = false;
                                    break;
                                }
                            }
                            if ok {
                                pos += len;
                                pc += 1;
                                continue 'main;
                            }
                        }
                    }
                }
                Inst::Assert(kind) => {
                    if self.eval_assert(*kind, pos, ctx) {
                        pc += 1;
                        continue 'main;
                    }
                }
                Inst::Look { prog, behind, neg } => {
                    let matched = if !*behind {
                        let ll = if ctx.transparent {
                            ctx.input.cps.len()
                        } else {
                            ctx.region_end
                        };
                        self.run(*prog, pos, ll, caps.clone(), ctx, None)
                    } else {
                        // ART's opaque bounds still let a look-behind see
                        // characters before the region start (probed on the
                        // API-34 emulator); only look-ahead is clamped at the
                        // region end.
                        let mut found = None;
                        let mut s = 0;
                        while s <= pos {
                            if let Some((_, nc)) =
                                self.run(*prog, s, pos, caps.clone(), ctx, Some(pos))
                            {
                                found = Some(nc);
                                break;
                            }
                            s += 1;
                        }
                        found.map(|nc| (pos, nc))
                    };
                    let ok = matched.is_some();
                    if *neg {
                        if !ok {
                            pc += 1;
                            continue 'main;
                        }
                    } else if let Some((_, nc)) = matched {
                        caps = nc;
                        pc += 1;
                        continue 'main;
                    }
                }
                Inst::Atomic(sub) => {
                    if let Some((end, nc)) = self.run(*sub, pos, limit, caps.clone(), ctx, None) {
                        pos = end;
                        caps = nc;
                        pc += 1;
                        continue 'main;
                    }
                }
            }
            match stack.pop() {
                Some((npc, npos, ncaps)) => {
                    pc = npc;
                    pos = npos;
                    caps = ncaps;
                }
                None => return None,
            }
        }
    }

    fn eval_assert(&self, kind: AssertKind, pos: usize, ctx: &Ctx) -> bool {
        match kind {
            AssertKind::StartInput => pos == 0,
            AssertKind::EndInput => {
                ctx.require_end.set(true);
                let end = ctx.input.cps.len();
                if pos == end {
                    ctx.hit_end.set(true);
                }
                pos == end
            }
            AssertKind::EndInputOrFinalNewline => {
                ctx.require_end.set(true);
                let end = ctx.input.cps.len();
                let ok = pos == end || is_final_newline(ctx.input, pos, end);
                if ok {
                    ctx.hit_end.set(true);
                }
                ok
            }
            AssertKind::StartLine(multiline) => {
                if multiline {
                    pos == ctx.effective_start()
                        || (pos > 0 && is_line_terminator(ctx.input.cps[pos - 1]))
                } else {
                    pos == ctx.effective_start()
                }
            }
            AssertKind::EndLine(multiline) => {
                let end = ctx.effective_end();
                if multiline {
                    ctx.require_end.set(true);
                    if pos == end {
                        ctx.hit_end.set(true);
                    }
                    pos == end || (pos < end && is_line_terminator(ctx.input.cps[pos]))
                } else {
                    ctx.require_end.set(true);
                    let ok = pos == end || is_final_newline(ctx.input, pos, end);
                    if ok {
                        ctx.hit_end.set(true);
                    }
                    ok
                }
            }
            AssertKind::WordBoundary => ctx.word_boundary(pos),
            AssertKind::NotWordBoundary => !ctx.word_boundary(pos),
            AssertKind::PrevMatchEnd => pos == ctx.prev_match_end,
        }
    }
}

/// One match attempt; `require_end` makes the match fill `[start, region_end)`.
fn attempt(
    pat: &Pattern,
    ctx: &Ctx,
    start: usize,
    require_end: bool,
) -> Option<(usize, Vec<Option<usize>>)> {
    let mut caps = vec![None; pat.caps_len];
    let r = pat.run(0, start, ctx.region_end, caps, ctx, None)?;
    caps = r.1;
    let end = r.0;
    if require_end && end != ctx.region_end {
        return None;
    }
    caps[0] = Some(start);
    caps[1] = Some(end);
    Some((end, caps))
}

fn find_from(pat: &Pattern, ctx: &Ctx, from_cp: usize) -> Option<(usize, Vec<Option<usize>>)> {
    let mut s = from_cp;
    while s <= ctx.region_end {
        if let Some(r) = attempt(pat, ctx, s, false) {
            return Some(r);
        }
        s += 1;
    }
    None
}

// ===========================================================================
// Registry (device-like opaque handles)
// ===========================================================================

struct Registry {
    next: u64,
    patterns: HashMap<u64, Arc<Pattern>>,
    matchers: HashMap<u64, MatcherState>,
    /// Pattern source + flags per handle, so a snapshot restore can recompile
    /// the parsed pattern instead of carrying a stale native handle.
    sources: HashMap<u64, (String, u32)>,
}

struct MatcherState {
    pattern: u64,
    input: Input,
    /// Region bounds in code-point units.
    region_start: usize,
    region_end: usize,
    anchoring: bool,
    transparent: bool,
    /// Previous match end (code-point units) for `\G`.
    prev_match_end: usize,
    next_search_cp: usize,
    last: Option<(usize, usize)>, // code-point units
    hit_end: bool,
    require_end: bool,
}

impl MatcherState {
    fn ctx(&self, prev_match_end_cp: usize) -> Ctx<'_> {
        Ctx {
            input: &self.input,
            region_start: self.region_start,
            region_end: self.region_end,
            anchoring: self.anchoring,
            transparent: self.transparent,
            prev_match_end: prev_match_end_cp,
            hit_end: Cell::new(false),
            require_end: Cell::new(false),
            steps: Cell::new(0),
            budget_hit: Cell::new(false),
        }
    }
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
/// A pattern table decoded from a snapshot but not yet recompiled. It is
/// installed when the registry is first created in a cold process.
static STAGED_REGEX: Mutex<Option<RegexState>> = Mutex::new(None);

/// Serialisable regex registry state: the handle counter and every compiled
/// pattern's source + flags. Matchers are transient (recreated by
/// `Pattern.matcher`) and are not carried.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RegexState {
    pub next: u64,
    pub patterns: Vec<(u64, String, u32)>,
}

fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| {
        let mut reg = Registry {
            next: 1,
            patterns: HashMap::new(),
            matchers: HashMap::new(),
            sources: HashMap::new(),
        };
        if let Some(state) = STAGED_REGEX
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            reg.next = state.next;
            for (h, source, flags) in state.patterns {
                if let Ok(p) = Parser::new(source.chars().collect()).parse(flags) {
                    reg.patterns.insert(h, Arc::new(p));
                    reg.sources.insert(h, (source, flags));
                }
            }
        }
        Mutex::new(reg)
    })
}

/// Stage a regex state decoded from a snapshot. Only a cold process (whose
/// registry is not yet initialised) imports it; an in-process restore shares
/// the live registry produced by the template boot.
///
/// The snapshot call sites are uncommitted at HEAD, so a clean HEAD build sees
/// this as dead code; keep the attribute so `clippy -D warnings` stays green.
#[allow(dead_code)]
pub(crate) fn stage_regex_state(state: RegexState) {
    if REGISTRY.get().is_some() || state.patterns.is_empty() {
        return;
    }
    *STAGED_REGEX.lock().unwrap_or_else(|e| e.into_inner()) = Some(state);
}

/// The live regex registry state, or the staged state when the registry has not
/// been initialised yet. `Vm::snapshot` serialises this.
#[allow(dead_code)]
pub(crate) fn current_regex_state() -> RegexState {
    if let Some(m) = REGISTRY.get() {
        let reg = m.lock().unwrap_or_else(|e| e.into_inner());
        let mut patterns: Vec<(u64, String, u32)> = reg
            .sources
            .iter()
            .map(|(h, (s, f))| (*h, s.clone(), *f))
            .collect();
        patterns.sort_by_key(|(h, _, _)| *h);
        return RegexState {
            next: reg.next,
            patterns,
        };
    }
    STAGED_REGEX
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

fn next_handle(reg: &mut Registry) -> u64 {
    let h = reg.next;
    reg.next += 1;
    h
}

// ===========================================================================
// Natives
// ===========================================================================

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let p = "Lcom/android/icu/util/regex/PatternNative;";
    vm.register_native(p, "compileImpl", "(Ljava/lang/String;I)J", compile_impl);
    vm.register_native(p, "getNativeFinalizer", "()J", native_finalizer);
    vm.register_native(p, "openMatcherImpl", "(J)J", open_matcher_impl);
    vm.register_native(
        p,
        "getMatchedGroupIndexImpl",
        "(JLjava/lang/String;)I",
        get_matched_group_index_impl,
    );

    let m = "Lcom/android/icu/util/regex/MatcherNative;";
    vm.register_native(m, "findImpl", "(JI[I)Z", find_impl);
    vm.register_native(m, "findNextImpl", "(J[I)Z", find_next_impl);
    vm.register_native(m, "getNativeFinalizer", "()J", native_finalizer);
    vm.register_native(m, "groupCountImpl", "(J)I", group_count_impl);
    vm.register_native(m, "hitEndImpl", "(J)Z", hit_end_impl);
    vm.register_native(m, "lookingAtImpl", "(J[I)Z", looking_at_impl);
    vm.register_native(m, "matchesImpl", "(J[I)Z", matches_impl);
    vm.register_native(m, "requireEndImpl", "(J)Z", require_end_impl);
    vm.register_native(
        m,
        "setInputImpl",
        "(JLjava/lang/String;II)V",
        set_input_impl,
    );
    vm.register_native(
        m,
        "useAnchoringBoundsImpl",
        "(JZ)V",
        use_anchoring_bounds_impl,
    );
    vm.register_native(
        m,
        "useTransparentBoundsImpl",
        "(JZ)V",
        use_transparent_bounds_impl,
    );
    Ok(())
}

fn native_finalizer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // No native memory to free; the Rust registry entries are reclaimed with
    // the process. Returning 0 tells NativeAllocationRegistry there is nothing
    // to release.
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn compile_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let units = vm
        .str_units(vm.arg_ref(ctx, 0))
        .map(|s| s.to_vec())
        .unwrap_or_default();
    let pattern_str = String::from_utf16_lossy(&units);
    let flags = vm.arg_i32(ctx, 1) as u32;
    let parsed = Parser::new(pattern_str.chars().collect()).parse(flags);
    match parsed {
        Ok(p) => {
            let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
            let h = next_handle(&mut reg);
            reg.patterns.insert(h, Arc::new(p));
            reg.sources.insert(h, (pattern_str, flags));
            drop(reg);
            vm.ret_i64(ctx, h as i64);
            Ok(())
        }
        Err(PErr::Syntax(desc, index)) => {
            Err(pattern_syntax_error(vm, &units, &desc, index as i32))
        }
        Err(PErr::Unsupported(what, index)) => Err(VmError::unsupported_api(
            format!("UNSUPPORTED_REGEX: {what} (index {index})"),
            "com.android.icu.util.regex.PatternNative.compileImpl".to_string(),
        )),
    }
}

fn pattern_syntax_error(vm: &mut Vm, pattern: &[u16], desc: &str, index: i32) -> VmError {
    let class = "Ljava/util/regex/PatternSyntaxException;";
    let attempt = (|| -> VmResult<Handle> {
        let cid = vm.load_class(class)?;
        let obj = vm.alloc_instance(cid)?;
        vm.native_roots.push(obj);
        let desc_h = vm.new_string(desc)?;
        vm.native_roots.push(desc_h);
        let pat_h = vm.new_string_utf16(pattern.to_vec())?;
        vm.native_roots.push(pat_h);
        let ctor = vm.find_method(
            cid,
            "<init>",
            "(Ljava/lang/String;Ljava/lang/String;I)V",
            InvokeKind::Direct,
        )?;
        let result =
            crate::interp::call_method(vm, ctor, obj, &[desc_h as u64, pat_h as u64, index as u64]);
        vm.native_roots.pop();
        vm.native_roots.pop();
        vm.native_roots.pop();
        result?;
        Ok(obj)
    })();
    match attempt {
        Ok(h) => VmError::JavaException(h),
        Err(e) => e,
    }
}

fn open_matcher_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ph = vm.arg_u64(ctx, 0);
    {
        let reg = registry().lock().unwrap_or_else(|p| p.into_inner());
        if !reg.patterns.contains_key(&ph) {
            return Err(VmError::unsupported_api(
                format!("UNSUPPORTED_REGEX: unknown pattern handle {ph}"),
                "com.android.icu.util.regex.PatternNative.openMatcherImpl".to_string(),
            ));
        }
    }
    let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    let h = next_handle(&mut reg);
    reg.matchers.insert(
        h,
        MatcherState {
            pattern: ph,
            input: Input::build(&[]),
            region_start: 0,
            region_end: 0,
            anchoring: true,
            transparent: false,
            prev_match_end: 0,
            next_search_cp: 0,
            last: None,
            hit_end: false,
            require_end: false,
        },
    );
    drop(reg);
    vm.ret_i64(ctx, h as i64);
    Ok(())
}

fn get_matched_group_index_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ph = vm.arg_u64(ctx, 0);
    let name = vm.string_to_rust(vm.arg_ref(ctx, 1)).unwrap_or_default();
    let reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    let idx = reg
        .patterns
        .get(&ph)
        .and_then(|p| p.names.get(&name).copied())
        .map(|i| i as i32)
        .unwrap_or(-1);
    vm.ret_i32(ctx, idx);
    Ok(())
}

fn group_count_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_u64(ctx, 0);
    let reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    let n = reg
        .matchers
        .get(&mh)
        .and_then(|m| reg.patterns.get(&m.pattern))
        .map(|p| p.group_count as i32)
        .unwrap_or(0);
    vm.ret_i32(ctx, n);
    Ok(())
}

fn hit_end_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_u64(ctx, 0);
    let reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    let v = reg.matchers.get(&mh).map(|m| m.hit_end).unwrap_or(false);
    vm.ret_bool(ctx, v);
    Ok(())
}

fn require_end_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_u64(ctx, 0);
    let reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    let v = reg
        .matchers
        .get(&mh)
        .map(|m| m.require_end)
        .unwrap_or(false);
    vm.ret_bool(ctx, v);
    Ok(())
}

fn set_input_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_u64(ctx, 0);
    let units = vm
        .str_units(vm.arg_ref(ctx, 1))
        .map(|s| s.to_vec())
        .unwrap_or_default();
    let start_u16 = vm.arg_i32(ctx, 2).max(0) as usize;
    let end_u16 = vm.arg_i32(ctx, 3).max(0) as usize;
    let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(m) = reg.matchers.get_mut(&mh) {
        m.input = Input::build(&units);
        let len = m.input.units.len();
        let start_u16 = start_u16.min(len);
        let end_u16 = end_u16.min(len).max(start_u16);
        let start_cp = m.input.u16_to_cp[start_u16];
        let end_cp = m.input.u16_to_cp[end_u16];
        m.region_start = start_cp;
        m.region_end = end_cp;
        m.prev_match_end = start_cp;
        m.next_search_cp = start_cp;
        m.last = None;
        m.hit_end = false;
        m.require_end = false;
    }
    drop(reg);
    Ok(())
}

fn use_anchoring_bounds_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_u64(ctx, 0);
    let v = vm.arg_bool(ctx, 1);
    let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(m) = reg.matchers.get_mut(&mh) {
        m.anchoring = v;
    }
    drop(reg);
    Ok(())
}

fn use_transparent_bounds_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_u64(ctx, 0);
    let v = vm.arg_bool(ctx, 1);
    let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(m) = reg.matchers.get_mut(&mh) {
        m.transparent = v;
    }
    drop(reg);
    Ok(())
}

/// Which `native` invoked the shared matcher.
#[derive(Clone, Copy)]
enum Mode {
    /// `find(int startIndex, ...)`: `startIndex` is a UTF-16 index.
    Find(usize),
    FindNext,
    Matches,
    LookingAt,
}

fn offsets_for(
    input: &Input,
    pat: &Pattern,
    caps: &[Option<usize>],
    start: usize,
    end: usize,
) -> Vec<i32> {
    let g = pat.group_count;
    let mut out = vec![-1i32; (g + 1) * 2];
    out[0] = input.cp_to_u16[start] as i32;
    out[1] = input.cp_to_u16[end] as i32;
    for i in 1..=g {
        if let (Some(s), Some(e)) = (
            caps.get(2 * i).copied().flatten(),
            caps.get(2 * i + 1).copied().flatten(),
        ) {
            out[2 * i] = input.cp_to_u16[s] as i32;
            out[2 * i + 1] = input.cp_to_u16[e] as i32;
        }
    }
    out
}

fn write_offsets(vm: &mut Vm, h: Handle, vals: &[i32]) {
    if let Some(Object::Array(ArrayObj {
        data: ArrayData::Int(v),
        ..
    })) = vm.heap.get_mut(h)
    {
        for (i, val) in vals.iter().enumerate() {
            if i < v.len() {
                v[i] = *val;
            }
        }
    }
}

/// Run the matcher, returning `(found, offsets, hit_end, require_end)`.
fn exec_match(mh: u64, mode: Mode) -> VmResult<(bool, Vec<i32>, bool, bool)> {
    let pat = {
        let reg = registry().lock().unwrap_or_else(|p| p.into_inner());
        match reg.matchers.get(&mh) {
            Some(m) => reg.patterns.get(&m.pattern).cloned(),
            None => None,
        }
    };
    let pat = match pat {
        Some(p) => p,
        None => return Ok((false, Vec::new(), false, false)),
    };
    let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    let m = match reg.matchers.get_mut(&mh) {
        Some(m) => m,
        None => return Ok((false, Vec::new(), false, false)),
    };
    let (search_cp, _require_end, prev) = match mode {
        Mode::Find(start_u16) => {
            let s = m
                .input
                .u16_to_cp
                .get(start_u16)
                .copied()
                .unwrap_or(m.input.cps.len());
            (s, false, s)
        }
        Mode::FindNext => (m.next_search_cp, false, m.prev_match_end),
        Mode::Matches => (m.region_start, true, m.region_start),
        Mode::LookingAt => (m.region_start, false, m.region_start),
    };
    let (r, hit, req, budget) = {
        let c = m.ctx(prev);
        let r = match mode {
            Mode::Matches => attempt(&pat, &c, search_cp, true),
            Mode::LookingAt => attempt(&pat, &c, search_cp, false),
            Mode::Find(_) | Mode::FindNext => find_from(&pat, &c, search_cp),
        };
        (r, c.hit_end.get(), c.require_end.get(), c.budget_hit.get())
    };
    m.hit_end = hit;
    m.require_end = req;
    if budget {
        return Err(VmError::unsupported_api(
            format!(
                "UNSUPPORTED_REGEX: match step budget exceeded ({} steps)",
                max_match_steps()
            ),
            "com.android.icu.util.regex.MatcherNative".to_string(),
        ));
    }
    match r {
        Some((end, caps)) => {
            let mstart = caps.first().copied().flatten().unwrap_or(search_cp);
            let mend = caps.get(1).copied().flatten().unwrap_or(end);
            let offs = offsets_for(&m.input, &pat, &caps, mstart, mend);
            m.last = Some((mstart, mend));
            m.prev_match_end = mend;
            m.next_search_cp = if mend > mstart { mend } else { mstart + 1 };
            Ok((true, offs, hit, req))
        }
        None => {
            m.last = None;
            Ok((false, Vec::new(), hit, req))
        }
    }
}

fn find_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_u64(ctx, 0);
    let start = vm.arg_i32(ctx, 1).max(0) as usize;
    let offs = vm.arg_ref(ctx, 2);
    let (found, vals, _, _) = exec_match(mh, Mode::Find(start))?;
    if found {
        write_offsets(vm, offs, &vals);
    }
    vm.ret_bool(ctx, found);
    Ok(())
}

fn find_next_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_u64(ctx, 0);
    let offs = vm.arg_ref(ctx, 1);
    let (found, vals, _, _) = exec_match(mh, Mode::FindNext)?;
    if found {
        write_offsets(vm, offs, &vals);
    }
    vm.ret_bool(ctx, found);
    Ok(())
}

fn matches_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_u64(ctx, 0);
    let offs = vm.arg_ref(ctx, 1);
    let (found, vals, _, _) = exec_match(mh, Mode::Matches)?;
    if found {
        write_offsets(vm, offs, &vals);
    }
    vm.ret_bool(ctx, found);
    Ok(())
}

fn looking_at_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_u64(ctx, 0);
    let offs = vm.arg_ref(ctx, 1);
    let (found, vals, _, _) = exec_match(mh, Mode::LookingAt)?;
    if found {
        write_offsets(vm, offs, &vals);
    }
    vm.ret_bool(ctx, found);
    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_for<'a>(input: &'a Input) -> Ctx<'a> {
        Ctx {
            input,
            region_start: 0,
            region_end: input.cps.len(),
            anchoring: true,
            transparent: false,
            prev_match_end: 0,
            hit_end: Cell::new(false),
            require_end: Cell::new(false),
            steps: Cell::new(0),
            budget_hit: Cell::new(false),
        }
    }

    fn compile(pat: &str, flags: u32) -> Pattern {
        Parser::new(pat.chars().collect())
            .parse(flags)
            .unwrap_or_else(|e| {
                panic!("pattern {pat:?} failed to parse: {e:?}");
            })
    }

    fn m(pat: &str, text: &str) -> Option<Vec<Option<usize>>> {
        let p = compile(pat, 0);
        let input = Input::build(&text.encode_utf16().collect::<Vec<u16>>());
        let ctx = ctx_for(&input);
        find_from(&p, &ctx, 0).map(|(_, caps)| caps)
    }

    fn group_text(input: &Input, caps: &[Option<usize>], i: usize) -> Option<String> {
        let s = caps[2 * i]?;
        let e = caps[2 * i + 1]?;
        let a = input.cp_to_u16[s];
        let b = input.cp_to_u16[e];
        Some(String::from_utf16_lossy(&input.units[a..b]))
    }

    #[test]
    fn literal_and_star() {
        assert!(m("a*b", "aaab").is_some());
        assert!(m("a*b", "b").is_some());
        assert!(m("a*b", "aaac").is_none());
    }

    #[test]
    fn capture_groups() {
        let p = compile("(a+)(b+)", 0);
        let input = Input::build(&"aaabbb".encode_utf16().collect::<Vec<u16>>());
        let ctx = ctx_for(&input);
        let (_, caps) = find_from(&p, &ctx, 0).unwrap();
        assert_eq!(caps[2], Some(0));
        assert_eq!(caps[3], Some(3));
        assert_eq!(caps[4], Some(3));
        assert_eq!(caps[5], Some(6));
        assert_eq!(group_text(&input, &caps, 1).as_deref(), Some("aaa"));
        assert_eq!(group_text(&input, &caps, 2).as_deref(), Some("bbb"));
    }

    #[test]
    fn backreference() {
        assert!(m(r"(ab)\1", "abab").is_some());
        assert!(m(r"(ab)\1", "abac").is_none());
    }

    #[test]
    fn lookahead() {
        assert!(m("a(?=b)", "ab").is_some());
        assert!(m("a(?!b)", "ac").is_some());
        assert!(m("a(?!b)", "ab").is_none());
    }

    #[test]
    fn lookbehind() {
        assert!(m("(?<=a)b", "ab").is_some());
        assert!(m("(?<!a)b", "cb").is_some());
        assert!(m("(?<!a)b", "ab").is_none());
    }

    #[test]
    fn named_group() {
        let p = compile("(?<word>\\w+)", 0);
        assert_eq!(p.names.get("word"), Some(&1));
    }

    #[test]
    fn lazy_vs_greedy() {
        let greedy = compile("<.*>", 0);
        let input = Input::build(&"<a><b>".encode_utf16().collect::<Vec<u16>>());
        let ctx = ctx_for(&input);
        let (end, _) = find_from(&greedy, &ctx, 0).unwrap();
        assert_eq!(end, 6);
        let lazy = compile("<.*?>", 0);
        let ctx = ctx_for(&input);
        let (end, _) = find_from(&lazy, &ctx, 0).unwrap();
        assert_eq!(end, 3);
    }

    #[test]
    fn possessive() {
        // a*+a cannot match "aa" because the a*+ consumes both.
        assert!(m("a*+a", "aa").is_none());
        assert!(m("a*a", "aa").is_some());
    }

    #[test]
    fn class_intersection() {
        assert!(m("[a-z&&[^bc]]", "a").is_some());
        assert!(m("[a-z&&[^bc]]", "b").is_none());
    }

    #[test]
    fn inline_case_insensitive() {
        assert!(m("(?i)abc", "ABC").is_some());
        assert!(m("abc", "ABC").is_none());
    }

    #[test]
    fn anchors_multiline() {
        assert!(m("(?m)^b", "a\nb").is_some());
        assert!(m("^b", "a\nb").is_none());
    }

    // ---- full Unicode case folding -------------------------------------

    fn mf(pat: &str, flags: u32, text: &str) -> Option<Vec<Option<usize>>> {
        let p = compile(pat, flags);
        let input = Input::build(&text.encode_utf16().collect::<Vec<u16>>());
        let ctx = ctx_for(&input);
        find_from(&p, &ctx, 0).map(|(_, caps)| caps)
    }

    fn find_text(pat: &str, flags: u32, text: &str) -> Option<String> {
        let p = compile(pat, flags);
        let input = Input::build(&text.encode_utf16().collect::<Vec<u16>>());
        let ctx = ctx_for(&input);
        let (_, caps) = find_from(&p, &ctx, 0)?;
        let s = caps[0]?;
        let e = caps[1]?;
        let a = input.cp_to_u16[s];
        let b = input.cp_to_u16[e];
        Some(String::from_utf16_lossy(&input.units[a..b]))
    }

    #[test]
    fn full_case_folding_expands_pattern() {
        // Pattern sharp-s folds to "ss" and consumes two input code points.
        assert!(mf("\u{00df}", F_CASE_INSENSITIVE, "ss").is_some());
        assert_eq!(
            find_text("\u{00df}", F_CASE_INSENSITIVE, "SS").as_deref(),
            Some("SS")
        );
        assert!(mf("\u{fb01}", F_CASE_INSENSITIVE, "fi").is_some());
        assert!(mf("\u{212a}", F_CASE_INSENSITIVE, "k").is_some());
        assert!(mf("\u{03c2}", F_CASE_INSENSITIVE, "\u{03a3}").is_some());
        assert!(mf("stra\u{00df}e", F_CASE_INSENSITIVE, "STRASSE").is_some());
    }

    #[test]
    fn full_case_folding_expands_input() {
        // Pattern "ss" matches a single sharp-s input code point.
        assert!(mf("ss", F_CASE_INSENSITIVE, "\u{00df}").is_some());
        assert!(mf("fi", F_CASE_INSENSITIVE, "\u{fb01}").is_some());
        assert!(mf("ss", F_CASE_INSENSITIVE, "s").is_none());
    }

    #[test]
    fn case_folding_is_not_simple_lowercase() {
        // A lone 's' must not match sharp-s, and sharp-s must not match 's'.
        assert!(mf("\u{00df}", F_CASE_INSENSITIVE, "s").is_none());
        assert!(mf("s", F_CASE_INSENSITIVE, "\u{00df}").is_none());
        assert!(mf("\u{00df}", 0, "ss").is_none());
    }

    #[test]
    fn case_insensitive_classes_and_negation() {
        assert!(mf("[a-z]", F_CASE_INSENSITIVE, "Z").is_some());
        assert!(mf("[^a]", F_CASE_INSENSITIVE, "A").is_none());
        assert!(mf("[^a]", F_CASE_INSENSITIVE, "B").is_some());
        assert!(mf("[\u{00df}]", F_CASE_INSENSITIVE, "\u{1e9e}").is_some());
        assert!(mf("[\u{00df}]", F_CASE_INSENSITIVE, "s").is_none());
    }

    // ---- Unicode properties --------------------------------------------

    #[test]
    fn general_categories() {
        assert!(m("\\p{Nd}", "\u{0660}").is_some());
        assert!(m("\\p{Nd}", "a").is_none());
        assert!(m("\\p{Lu}", "\u{00c9}").is_some());
        assert!(m("\\p{Ll}", "\u{00e9}").is_some());
        assert!(m("\\p{L}", "\u{5b57}").is_some());
        assert!(m("\\p{No}", "\u{00bd}").is_some());
        assert!(m("\\p{Sm}", "+").is_some());
        assert!(m("\\P{L}", "5").is_some());
    }

    #[test]
    fn unicode_digit_is_decimal_only() {
        assert!(m("\\d", "\u{0660}").is_some());
        assert!(m("\\d", "\u{2160}").is_none()); // Roman numeral: Nl, not Nd
        assert!(m("\\d", "\u{00bd}").is_none()); // vulgar fraction: No
    }

    #[test]
    fn script_properties() {
        assert!(m("\\p{sc=Greek}", "\u{03b1}").is_some());
        assert!(m("\\p{IsGreek}", "\u{03b1}").is_some());
        assert!(m("\\p{Greek}", "\u{03b1}").is_some());
        assert!(m("\\p{sc=Latin}", "a").is_some());
        assert!(m("\\p{sc=Greek}", "a").is_none());
    }

    #[test]
    fn java_whitespace_differs_from_unicode_white_space() {
        assert!(m("\\p{javaWhitespace}", " ").is_some());
        assert!(m("\\p{javaWhitespace}", "\u{00a0}").is_none());
        assert!(m("\\p{javaWhitespace}", "\u{0085}").is_none());
        assert!(m("\\p{White_Space}", "\u{00a0}").is_some());
    }

    #[test]
    fn posix_classes() {
        assert!(m("[[:alpha:]]", "\u{00e9}").is_some());
        assert!(m("[[:digit:]]", "\u{0660}").is_some());
        assert!(m("[[:xdigit:]]", "\u{ff10}").is_some());
        assert!(m("[[:xdigit:]]", "g").is_none());
        assert!(m("[[:blank:]]", "\u{00a0}").is_some());
        assert!(m("[[:cntrl:]]", "\u{0001}").is_some());
    }

    // ---- flags / structural fixes --------------------------------------

    #[test]
    fn literal_flag() {
        assert!(mf("a.b", F_LITERAL, "a.b").is_some());
        assert!(mf("a.b", F_LITERAL, "axb").is_none());
        assert!(mf("a*b", F_LITERAL, "a*b").is_some());
        assert!(mf("\u{00df}", F_LITERAL | F_CASE_INSENSITIVE, "SS").is_some());
    }

    #[test]
    fn capturing_groups_numbered_outer_first() {
        let p = compile("(a(b(c)))", 0);
        let input = Input::build(&"abc".encode_utf16().collect::<Vec<u16>>());
        let ctx = ctx_for(&input);
        let (_, caps) = find_from(&p, &ctx, 0).unwrap();
        assert_eq!(group_text(&input, &caps, 1).as_deref(), Some("abc"));
        assert_eq!(group_text(&input, &caps, 2).as_deref(), Some("bc"));
        assert_eq!(group_text(&input, &caps, 3).as_deref(), Some("c"));
    }

    #[test]
    fn bounded_variable_lookbehind_uses_alternation() {
        assert!(m("(?<=ab|abc)d", "abcd").is_some());
        assert!(m("(?<=a{1,3})b", "aaab").is_some());
    }

    #[test]
    fn unbounded_lookbehind_is_rejected() {
        assert!(Parser::new("(?<=a+)b".chars().collect()).parse(0).is_err());
        assert!(Parser::new("(?<=a*)b".chars().collect()).parse(0).is_err());
        assert!(Parser::new("(?<=a{2,})b".chars().collect())
            .parse(0)
            .is_err());
    }

    #[test]
    fn unknown_inline_flag_is_a_syntax_error() {
        assert!(Parser::new("(?U)\\w".chars().collect()).parse(0).is_err());
    }

    #[test]
    fn empty_loop_cannot_spin_forever() {
        // The nested star would loop forever without the hidden loop slot.
        assert!(m("(a*)*", "aaa").is_some());
        assert!(m("(?:)*", "aaa").is_some());
        assert!(m("(?:a?)*", "aaa").is_some());
    }

    #[test]
    fn backtracking_step_budget_is_bounded() {
        // A pathological but finite pattern must complete under the step budget
        // rather than hang.
        let p = compile("(a+)+$", 0);
        let input = Input::build(
            &"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa!"
                .encode_utf16()
                .collect::<Vec<u16>>(),
        );
        let ctx = ctx_for(&input);
        let _ = attempt(&p, &ctx, 0, true);
    }
}
