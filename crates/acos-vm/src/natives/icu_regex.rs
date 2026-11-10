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
//! Supported (verified against ART in `fixtures/expected-art/dexprogs`):
//! literals, `.` (with `DOTALL`/`UNIX_LINES`), `\d \D \w \W \s \S \h \H \v \V`,
//! character classes/ranges/negation/intersection (`&&`), POSIX and `\p{...}`
//! properties (the subset resolved below), greedy/lazy/possessive quantifiers,
//! `{n}`, `{n,}`, `{n,m}`, capturing / non-capturing / named groups, `(?i)`
//! inline flags, alternation, backreferences (`\1`..`\n`, `\k<name>`),
//! lookahead / lookbehind (positive and negative), atomic groups, `^ $ \A \z
//! \Z \b \B \G \R`, `\Q..\E`, hex / unicode / octal / control escapes, region
//! bounds, anchoring bounds and transparent bounds.
//!
//! Constructs ICU accepts but this engine does not are reported as structured
//! `UNSUPPORTED_JAVA_API` (never silently mis-matched). Patterns that are
//! invalid *per ICU* raise a real `java.util.regex.PatternSyntaxException` with
//! a description, the pattern and an approximate 0-based index.
//!
//! Known honest approximations (documented, not silent):
//! * `CASE_INSENSITIVE` uses simple Unicode case folding via Rust's
//!   `to_lowercase`/`to_uppercase` first-scalar mapping; the handful of ICU
//!   special folds (e.g. Kelvin sign as `k`) are not modelled.
//! * Unicode properties are approximated with the standard library's `char`
//!   predicates (`is_alphabetic`, `is_numeric`, ...); the exact general
//!   category tables are not embedded.
//! * `hitEnd()`/`requireEnd()` are best-effort: `hitEnd` is set when a
//!   consuming instruction reaches the match limit, and `requireEnd` mirrors it
//!   for a successful match.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

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
const F_DOTALL: u32 = 0x20;

// ===========================================================================
// Instruction set
// ===========================================================================

#[derive(Clone, Debug)]
enum Inst {
    /// Match one code point (with optional simple case folding).
    Char(u32, bool),
    /// Match one code point against a character class.
    Class(u16),
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
    Alpha,
    Upper,
    Lower,
    Alnum,
    Punct,
    Graph,
    Print,
    Blank,
    Cntrl,
    XDigit,
    Latin,
    Greek,
    JavaLower,
    JavaUpper,
    JavaWhitespace,
}

impl ClassNode {
    fn contains(&self, c: u32) -> bool {
        match self {
            ClassNode::Empty => false,
            ClassNode::Range(a, b) => c >= *a && c <= *b,
            ClassNode::Union(v) => v.iter().any(|n| n.contains(c)),
            ClassNode::Inter(a, b) => a.contains(c) && b.contains(c),
            ClassNode::Neg(n) => !n.contains(c),
            ClassNode::Pred(p) => pred_match(*p, c),
        }
    }
}

fn pred_match(p: Pred, c: u32) -> bool {
    let ch = match char::from_u32(c) {
        Some(ch) => ch,
        None => return false,
    };
    match p {
        Pred::Digit => ch.is_numeric(),
        Pred::Word => is_word_cp(c),
        Pred::Space => ch.is_whitespace(),
        Pred::Horiz => matches!(c, 0x09 | 0x20 | 0xA0 | 0x1680 | 0x180E | 0x202F | 0x205F | 0x3000)
            || (0x2000..=0x200A).contains(&c),
        Pred::Vert => matches!(c, 0x0A | 0x0B | 0x0C | 0x0D | 0x85 | 0x2028 | 0x2029),
        Pred::Ascii => c <= 0x7F,
        Pred::Alpha => ch.is_alphabetic(),
        Pred::Upper | Pred::JavaUpper => ch.is_uppercase(),
        Pred::Lower | Pred::JavaLower => ch.is_lowercase(),
        Pred::Alnum => ch.is_alphanumeric(),
        Pred::Punct => is_punct(c),
        Pred::Graph => !ch.is_whitespace() && !ch.is_control(),
        Pred::Print => !ch.is_control(),
        Pred::Blank => c == 0x09 || c == 0x20,
        Pred::Cntrl => ch.is_control(),
        Pred::XDigit => ch.is_ascii_hexdigit() || ch.is_numeric(),
        Pred::Latin => is_latin(c),
        Pred::Greek => (0x0370..=0x03FF).contains(&c) || (0x1F00..=0x1FFF).contains(&c),
        Pred::JavaWhitespace => ch.is_whitespace(),
    }
}

fn is_punct(c: u32) -> bool {
    matches!(c,
        0x21..=0x23 | 0x25..=0x2A | 0x2C..=0x2F | 0x3A..=0x3B | 0x3F..=0x40
        | 0x5B..=0x5D | 0x5F | 0x7B | 0x7D | 0xA1 | 0xA7 | 0xAB | 0xB6 | 0xB7
        | 0xBB | 0xBF | 0x2010..=0x2027 | 0x2030..=0x205E)
}

fn is_latin(c: u32) -> bool {
    (0x0041..=0x005A).contains(&c)
        || (0x0061..=0x007A).contains(&c)
        || (0x00C0..=0x024F).contains(&c)
        || (0x1E00..=0x1EFF).contains(&c)
}

fn is_word_cp(c: u32) -> bool {
    c == 0x200C
        || c == 0x200D
        || char::from_u32(c).map(|ch| ch.is_alphanumeric() || ch == '_').unwrap_or(false)
}

// ===========================================================================
// Compiled pattern
// ===========================================================================

struct Pattern {
    /// Program list; index 0 is the main program.
    programs: Vec<Vec<Inst>>,
    classes: Vec<ClassNode>,
    group_count: usize,
    names: HashMap<String, usize>,
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
    group_count: usize,
    names: HashMap<String, usize>,
    depth: usize,
}

const MAX_DEPTH: usize = 200;

impl Parser {
    fn new(pat: Vec<char>) -> Parser {
        Parser {
            pat,
            pos: 0,
            classes: Vec::new(),
            programs: vec![Vec::new()],
            group_count: 0,
            names: HashMap::new(),
            depth: 0,
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

    fn parse(mut self, initial_flags: u32) -> Result<Pattern, PErr> {
        let mut flags = initial_flags;
        let main = self.parse_alt(&mut flags)?;
        if self.pos < self.pat.len() {
            return Err(PErr::Syntax("Unmatched closing ')'".to_string(), self.pos));
        }
        let mut main = main;
        main.push(Inst::Match);
        self.programs[0] = main;
        Ok(Pattern {
            programs: self.programs,
            classes: self.classes,
            group_count: self.group_count,
            names: self.names,
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
        loop {
            self.skip_comments(*flags);
            match self.peek() {
                None | Some('|') | Some(')') => break,
                _ => {
                    let atom = self.parse_quantified(flags)?;
                    splice(&mut buf, atom);
                }
            }
        }
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
        if q.kind == QKind::Possessive {
            let mut sub: Vec<Inst> = Vec::new();
            emit_repeat(&mut sub, &body, q.min, q.max, false)?;
            let idx = self.add_sub(sub);
            return Ok(vec![Inst::Atomic(idx)]);
        }
        let mut buf: Vec<Inst> = Vec::new();
        emit_repeat(&mut buf, &body, q.min, q.max, q.kind == QKind::Lazy)?;
        Ok(buf)
    }

    fn parse_atom(&mut self, flags: &mut u32) -> Result<Vec<Inst>, PErr> {
        let c = match self.peek() {
            Some(c) => c,
            None => return Err(PErr::Syntax("Unexpected end of pattern".to_string(), self.pos)),
        };
        match c {
            '(' => self.parse_group(flags),
            '[' => {
                let node = self.parse_class()?;
                Ok(vec![Inst::Class(self.push_class(node))])
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
                Ok(vec![Inst::Assert(AssertKind::StartLine(*flags & F_MULTILINE != 0))])
            }
            '$' => {
                self.pos += 1;
                Ok(vec![Inst::Assert(AssertKind::EndLine(*flags & F_MULTILINE != 0))])
            }
            '\\' => self.parse_escape(flags),
            ')' => Err(PErr::Syntax("Unmatched closing ')'".to_string(), self.pos)),
            '*' | '+' | '?' => Err(PErr::Syntax(
                format!("Dangling quantifier '{c}'"),
                self.pos,
            )),
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
                return self.parse_alt(&mut inner);
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
                    let body = self.parse_alt(&mut inner)?;
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
                    let body = self.parse_alt(&mut inner)?;
                    self.expect(')')?;
                    let sub = self.add_sub(body);
                    return Ok(vec![Inst::Look {
                        prog: sub,
                        behind: true,
                        neg: true,
                    }]);
                }
                let name = self.read_name('>')?;
                let mut inner = *flags;
                let body = self.parse_alt(&mut inner)?;
                self.expect(')')?;
                let idx = self.new_group();
                self.names.insert(name, idx);
                return Ok(wrap_capture(idx, body));
            }
            if self.eat('P') {
                if self.eat('<') {
                    let name = self.read_name('>')?;
                    let mut inner = *flags;
                    let body = self.parse_alt(&mut inner)?;
                    self.expect(')')?;
                    let idx = self.new_group();
                    self.names.insert(name, idx);
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
            let mut inner = *flags;
            let body = self.parse_alt(&mut inner)?;
            self.expect(')')?;
            let idx = self.new_group();
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
                        'u' => 0,
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
                    return Err(PErr::Unsupported(
                        "UNICODE_CHARACTER_CLASS inline flag".to_string(),
                        self.pos,
                    ))
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
        if name.is_empty() || name.chars().next().map(|c| !c.is_ascii_alphabetic()).unwrap_or(true) {
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
            return Ok(vec![Inst::Class(self.push_class(n))]);
        }
        match c {
            'b' => Ok(vec![Inst::Assert(AssertKind::WordBoundary)]),
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
                Ok(vec![Inst::Class(self.push_class(node))])
            }
            'k' => {
                if !self.eat('<') {
                    return Err(PErr::Syntax("Expected '<' after \\k".to_string(), self.pos));
                }
                let name = self.read_name('>')?;
                let idx = *self.names.get(&name).ok_or_else(|| {
                    PErr::Syntax(format!("Named backreference to unknown group '{name}'"), self.pos)
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
                let ch = self.bump().ok_or_else(|| {
                    PErr::Syntax("Illegal control escape".to_string(), self.pos)
                })?;
                if !ch.is_ascii_alphabetic() {
                    return Err(PErr::Syntax(
                        "Illegal control escape".to_string(),
                        self.pos,
                    ));
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
                None => {
                    return Err(PErr::Syntax(
                        "Unclosed property name".to_string(),
                        self.pos,
                    ))
                }
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
            return u32::from_str_radix(&s, 16)
                .map_err(|_| PErr::Syntax("Hexadecimal codepoint too large".to_string(), self.pos));
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

    fn parse_class(&mut self) -> Result<ClassNode, PErr> {
        self.expect('[')?;
        let neg = self.eat('^');
        let mut node = self.parse_class_body()?;
        while self.peek() == Some('&') && self.peek_at(1) == Some('&') {
            self.pos += 2;
            let rhs = if self.peek() == Some('[') {
                self.parse_class()?
            } else {
                self.parse_class_body()?
            };
            node = ClassNode::Inter(Box::new(node), Box::new(rhs));
        }
        self.expect(']')?;
        if neg {
            node = ClassNode::Neg(Box::new(node));
        }
        Ok(node)
    }

    fn parse_class_body(&mut self) -> Result<ClassNode, PErr> {
        let mut items: Vec<ClassNode> = Vec::new();
        let mut first = true;
        loop {
            if self.peek().is_none() {
                return Err(PErr::Syntax("Unclosed character class".to_string(), self.pos));
            }
            if self.peek() == Some(']') && !first {
                break;
            }
            if self.peek() == Some('&') && self.peek_at(1) == Some('&') {
                break;
            }
            let atom = self.parse_class_atom()?;
            first = false;
            match atom {
                ClassAtom::Single(c) => {
                    if self.peek() == Some('-') && self.peek_at(1).is_some() && self.peek_at(1) != Some(']') {
                        self.pos += 1;
                        match self.parse_class_atom()? {
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

    fn parse_class_atom(&mut self) -> Result<ClassAtom, PErr> {
        match self.peek() {
            Some('[') => Ok(ClassAtom::Node(self.parse_class()?)),
            Some('\\') => self.parse_class_escape(),
            Some(c) => {
                self.pos += 1;
                if c == ']' {
                    Ok(ClassAtom::Single(']' as u32))
                } else {
                    Ok(ClassAtom::Single(c as u32))
                }
            }
            None => Err(PErr::Syntax("Unclosed character class".to_string(), self.pos)),
        }
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
        _ => {}
    }
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
            splice(buf, body.to_vec());
            buf.push(Inst::Jmp(l1 as u32));
            let l3 = buf.len() as u32;
            buf[l1] = if lazy {
                Inst::Split(l3, bstart)
            } else {
                Inst::Split(bstart, l3)
            };
        }
    }
    Ok(())
}

fn resolve_property(name: &str) -> Option<ClassNode> {
    let lower = name.to_ascii_lowercase();
    let p = match lower.as_str() {
        "l" | "alpha" | "alphabetic" | "isalphabetic" => Pred::Alpha,
        "lu" | "uppercase" | "isuppercase" => Pred::Upper,
        "ll" | "lowercase" | "islowercase" => Pred::Lower,
        "n" | "nd" | "digit" | "isdigit" | "decimal_number" => Pred::Digit,
        "alnum" => Pred::Alnum,
        "space" | "whitespace" | "white_space" | "iswhite_space" => Pred::Space,
        "ascii" => Pred::Ascii,
        "punct" | "ispunctuation" => Pred::Punct,
        "graph" => Pred::Graph,
        "print" => Pred::Print,
        "blank" => Pred::Blank,
        "cntrl" => Pred::Cntrl,
        "xdigit" => Pred::XDigit,
        "islatin" | "latin" => Pred::Latin,
        "isgreek" | "greek" => Pred::Greek,
        "ingreek" => Pred::Greek,
        "javalowercase" => Pred::JavaLower,
        "javauppercase" => Pred::JavaUpper,
        "javawhitespace" => Pred::JavaWhitespace,
        _ => return None,
    };
    Some(ClassNode::Pred(p))
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
        self.input.cps.get(cp).map(|&c| is_word_cp(c)).unwrap_or(false)
    }

    fn word_boundary(&self, cp: usize) -> bool {
        let before = if cp == 0 { false } else { self.is_word_at(cp - 1) };
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

fn simple_fold_eq(a: char, b: char) -> bool {
    if a == b {
        return true;
    }
    let la = a.to_lowercase().next().unwrap_or(a);
    let lb = b.to_lowercase().next().unwrap_or(b);
    if la == lb {
        return true;
    }
    a.to_uppercase().next().unwrap_or(a) == b.to_uppercase().next().unwrap_or(b)
}

fn char_eq(a: u32, b: u32, ic: bool) -> bool {
    if a == b {
        return true;
    }
    if !ic {
        return false;
    }
    match (char::from_u32(a), char::from_u32(b)) {
        (Some(x), Some(y)) => simple_fold_eq(x, y),
        _ => false,
    }
}

fn class_contains(node: &ClassNode, c: u32, ic: bool) -> bool {
    if node.contains(c) {
        return true;
    }
    if !ic {
        return false;
    }
    if let Some(ch) = char::from_u32(c) {
        if let Some(l) = ch.to_lowercase().next() {
            if node.contains(l as u32) {
                return true;
            }
        }
        if let Some(u) = ch.to_uppercase().next() {
            if node.contains(u as u32) {
                return true;
            }
        }
    }
    false
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
    ) -> Option<(usize, Vec<Option<usize>>)> {
        let prog = &self.programs[prog_idx as usize];
        let mut pc = 0usize;
        let mut pos = start;
        let mut caps = caps_in;
        let mut stack: Vec<(usize, usize, Vec<Option<usize>>)> = Vec::new();
        'main: loop {
            match &prog[pc] {
                Inst::Match => return Some((pos, caps)),
                Inst::Char(c, ic) => {
                    if pos < limit && char_eq(ctx.input.cps[pos], *c, *ic) {
                        pos += 1;
                        pc += 1;
                        continue 'main;
                    }
                    ctx.hit_end.set(true);
                }
                Inst::Class(ci) => {
                    if pos < limit {
                        let node = &self.classes[*ci as usize];
                        if class_contains(node, ctx.input.cps[pos], false) {
                            pos += 1;
                            pc += 1;
                            continue 'main;
                        }
                    } else {
                        ctx.hit_end.set(true);
                    }
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
                        self.run(*prog, pos, ll, caps.clone(), ctx)
                    } else {
                        let lo = if ctx.transparent { 0 } else { ctx.region_start };
                        let mut found = None;
                        let mut s = lo;
                        while s <= pos {
                            if let Some((end, nc)) = self.run(*prog, s, pos, caps.clone(), ctx) {
                                if end == pos {
                                    found = Some(nc);
                                    break;
                                }
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
                    if let Some((end, nc)) = self.run(*sub, pos, limit, caps.clone(), ctx) {
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
                pos == ctx.input.cps.len()
            }
            AssertKind::EndInputOrFinalNewline => {
                ctx.require_end.set(true);
                pos == ctx.input.cps.len() || is_final_newline(ctx.input, pos, ctx.input.cps.len())
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
                    pos == end || (pos < end && is_line_terminator(ctx.input.cps[pos]))
                } else {
                    ctx.require_end.set(true);
                    pos == end || is_final_newline(ctx.input, pos, end)
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
    let mut caps = vec![None; (pat.group_count + 1) * 2];
    let r = pat.run(0, start, ctx.region_end, caps, ctx)?;
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
        }
    }
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| {
        Mutex::new(Registry {
            next: 1,
            patterns: HashMap::new(),
            matchers: HashMap::new(),
        })
    })
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
    vm.register_native(m, "setInputImpl", "(JLjava/lang/String;II)V", set_input_impl);
    vm.register_native(m, "useAnchoringBoundsImpl", "(JZ)V", use_anchoring_bounds_impl);
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
    let v = reg.matchers.get(&mh).map(|m| m.require_end).unwrap_or(false);
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
fn exec_match(mh: u64, mode: Mode) -> (bool, Vec<i32>, bool, bool) {
    let pat = {
        let reg = registry().lock().unwrap_or_else(|p| p.into_inner());
        match reg.matchers.get(&mh) {
            Some(m) => reg.patterns.get(&m.pattern).cloned(),
            None => None,
        }
    };
    let pat = match pat {
        Some(p) => p,
        None => return (false, Vec::new(), false, false),
    };
    let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    let m = match reg.matchers.get_mut(&mh) {
        Some(m) => m,
        None => return (false, Vec::new(), false, false),
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
    let (r, hit, req) = {
        let c = m.ctx(prev);
        let r = match mode {
            Mode::Matches => attempt(&pat, &c, search_cp, true),
            Mode::LookingAt => attempt(&pat, &c, search_cp, false),
            Mode::Find(_) | Mode::FindNext => find_from(&pat, &c, search_cp),
        };
        (r, c.hit_end.get(), c.require_end.get())
    };
    m.hit_end = hit;
    m.require_end = req;
    match r {
        Some((end, caps)) => {
            let mstart = caps.first().copied().flatten().unwrap_or(search_cp);
            let mend = caps.get(1).copied().flatten().unwrap_or(end);
            let offs = offsets_for(&m.input, &pat, &caps, mstart, mend);
            m.last = Some((mstart, mend));
            m.prev_match_end = mend;
            m.next_search_cp = if mend > mstart { mend } else { mstart + 1 };
            (true, offs, hit, req)
        }
        None => {
            m.last = None;
            (false, Vec::new(), hit, req)
        }
    }
}

fn find_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_u64(ctx, 0);
    let start = vm.arg_i32(ctx, 1).max(0) as usize;
    let offs = vm.arg_ref(ctx, 2);
    let (found, vals, _, _) = exec_match(mh, Mode::Find(start));
    if found {
        write_offsets(vm, offs, &vals);
    }
    vm.ret_bool(ctx, found);
    Ok(())
}

fn find_next_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_u64(ctx, 0);
    let offs = vm.arg_ref(ctx, 1);
    let (found, vals, _, _) = exec_match(mh, Mode::FindNext);
    if found {
        write_offsets(vm, offs, &vals);
    }
    vm.ret_bool(ctx, found);
    Ok(())
}

fn matches_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_u64(ctx, 0);
    let offs = vm.arg_ref(ctx, 1);
    let (found, vals, _, _) = exec_match(mh, Mode::Matches);
    if found {
        write_offsets(vm, offs, &vals);
    }
    vm.ret_bool(ctx, found);
    Ok(())
}

fn looking_at_impl(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_u64(ctx, 0);
    let offs = vm.arg_ref(ctx, 1);
    let (found, vals, _, _) = exec_match(mh, Mode::LookingAt);
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
        }
    }

    fn compile(pat: &str, flags: u32) -> Pattern {
        Parser::new(pat.chars().collect()).parse(flags).unwrap_or_else(|e| {
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
}
