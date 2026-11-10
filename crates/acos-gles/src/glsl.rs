//! A small GLSL ES 1.00 interpreter (ADR-0025).
//!
//! The substrate never JITs guest shader source and never executes guest
//! memory: a shader is tokenised, parsed into an AST and evaluated per vertex /
//! per fragment by this module. The accepted grammar is the subset the engines
//! we host actually upload (Skia's `glsl` backend, Impeller's legacy GL backend,
//! RN GL modules):
//!
//! * qualifiers `attribute`, `uniform`, `varying`, `const` and plain globals;
//! * scalar/vector/matrix types, constructors, swizzles, indexing;
//! * arithmetic, comparison and logical operators with GLSL precedence;
//! * `if`/`else`, `return`, `discard`, local declarations and assignments;
//! * **bounded `for` loops** (`for (init; cond; step) body`), including nested
//!   loops: a per-loop cap plus a per-invocation aggregate evaluation budget
//!   ([`SHADER_STEP_BUDGET`]) keep the work constant-bounded, and exceeding
//!   either is a structured runtime error (never a hang);
//! * user-defined functions (called by name/arity, with parameter scoping and
//!   `return`, recursing only within the instruction budget);
//! * the builtins `texture2D`, `abs/floor/ceil/fract/mod/min/max/clamp/mix/
//!   step/smoothstep/pow/exp/log/sqrt/inversesqrt/sin/cos/tan/atan/sign/
//!   length/distance/dot/cross/normalize/matrixCompMult`.
//!
//! Anything outside the subset (`while`/`do` loops, arrays, structs, the
//! preprocessor with conditional branches) is a **structured rejection**: the
//! compiler returns `accepted = false` plus a positioned reason, and the GL
//! entry point reports `GL_FALSE` from `glGetShaderiv(GL_COMPILE_STATUS)` and
//! logs `UNSUPPORTED_GLES`. A shader is never silently replaced by a default,
//! and no accepted construct returns a placeholder value.

/// Shader stage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(missing_docs)]
pub enum Stage {
    Vertex,
    Fragment,
}

impl Stage {
    /// Human-readable name used in diagnostics.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Stage::Vertex => "vertex",
            Stage::Fragment => "fragment",
        }
    }
}

/// A GLSL type in the supported subset.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(missing_docs)]
pub enum Ty {
    Void,
    Bool,
    Int,
    Float,
    Vec2,
    Vec3,
    Vec4,
    Mat2,
    Mat3,
    Mat4,
    Sampler2D,
}

impl Ty {
    /// Parse a type name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Ty> {
        Some(match name {
            "void" => Ty::Void,
            "bool" => Ty::Bool,
            "int" => Ty::Int,
            "float" => Ty::Float,
            "vec2" => Ty::Vec2,
            "vec3" => Ty::Vec3,
            "vec4" => Ty::Vec4,
            "mat2" => Ty::Mat2,
            "mat3" => Ty::Mat3,
            "mat4" => Ty::Mat4,
            "sampler2D" => Ty::Sampler2D,
            _ => return None,
        })
    }

    /// Component count for the vector/matrix types (0 for scalars/samplers).
    #[must_use]
    pub fn cols(self) -> usize {
        match self {
            Ty::Vec2 => 2,
            Ty::Vec3 => 3,
            Ty::Vec4 => 4,
            Ty::Mat2 => 2,
            Ty::Mat3 => 3,
            Ty::Mat4 => 4,
            _ => 0,
        }
    }

    /// Whether the type is a matrix.
    #[must_use]
    pub fn is_matrix(self) -> bool {
        matches!(self, Ty::Mat2 | Ty::Mat3 | Ty::Mat4)
    }
}

/// A runtime value of the interpreter.
///
/// Vectors and matrices are stored right-padded; `n` is the live extent, so a
/// `vec3` never bleeds into a fourth component of an arithmetic result.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `float`
    F(f32),
    /// `int`
    I(i32),
    /// `bool`
    B(bool),
    /// `vec2`/`vec3`/`vec4`
    V([f32; 4], usize),
    /// `mat2`/`mat3`/`mat4`, row-major, `n*n` live entries.
    M([f32; 16], usize),
    /// `sampler2D`: the texture unit number.
    S(i32),
}

impl Value {
    /// Build a `vecN` from the first `n` components of `v`.
    #[must_use]
    pub fn vec(v: [f32; 4], n: usize) -> Value {
        Value::V(v, n)
    }

    /// Build a `vec4`.
    #[must_use]
    pub fn vec4(x: f32, y: f32, z: f32, w: f32) -> Value {
        Value::V([x, y, z, w], 4)
    }

    /// Read the value as a `vec4`, broadcasting scalars.
    #[must_use]
    pub fn to_vec4(&self) -> [f32; 4] {
        match self {
            Value::F(f) => [*f; 4],
            Value::I(i) => [*i as f32; 4],
            Value::B(b) => [f32::from(u8::from(*b)); 4],
            Value::V(v, _) => *v,
            Value::M(m, n) => {
                let mut out = [0.0; 4];
                for (i, slot) in out.iter_mut().enumerate().take(*n) {
                    *slot = m[i];
                }
                out
            }
            Value::S(s) => [*s as f32; 4],
        }
    }

    /// First component as `f32`.
    #[must_use]
    pub fn to_f32(&self) -> f32 {
        self.to_vec4()[0]
    }

    /// Truthiness for conditionals.
    #[must_use]
    pub fn truthy(&self) -> bool {
        match self {
            Value::B(b) => *b,
            other => other.to_f32() != 0.0,
        }
    }

    /// Component count.
    #[must_use]
    pub fn width(&self) -> usize {
        match self {
            Value::F(_) | Value::I(_) | Value::B(_) | Value::S(_) => 1,
            Value::V(_, n) => *n,
            Value::M(_, n) => n * n,
        }
    }
}

/// Interpreter failure (a shader compiled but the program is not evaluable).
#[derive(Clone, Debug)]
pub struct EvalError(pub String);

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

type EvalResult<T> = Result<T, EvalError>;

/// Hard cap on `for`-loop iterations (a non-constant bound is an error).
const FOR_ITERATION_CAP: u32 = 4096;

/// Aggregate AST-evaluation budget for one shader invocation (one vertex or
/// one fragment).
///
/// `FOR_ITERATION_CAP` is *per loop*, so N nested `for` loops can execute
/// `4096^N` iterations; because a fragment shader runs per pixel that is an
/// unbounded CPU DoS from a tiny shader (W94-A #4). This budget is shared by
/// every statement/expression the invocation evaluates, so any loop nest is
/// bounded by a constant amount of work. A shader that exhausts it fails the
/// draw with a structured [`EvalError`] instead of hanging.
const SHADER_STEP_BUDGET: u32 = 1 << 16;

/// Maximum user-function call depth. The step budget alone bounds recursion in
/// *evaluations*, but each call still consumes Rust stack, so a self-recursive
/// shader could abort the process before the budget runs out. Exceeding this is
/// a structured error, like any other unsupported construct.
const MAX_CALL_DEPTH: u32 = 32;

fn err<T>(msg: impl Into<String>) -> EvalResult<T> {
    Err(EvalError(msg.into()))
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

/// Declaration qualifier.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(missing_docs)]
pub enum Qual {
    Plain,
    Attribute,
    Uniform,
    Varying,
    Const,
}

/// One declarator (a declaration of a single name).
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct Decl {
    pub qual: Qual,
    pub ty: Ty,
    pub name: String,
    pub init: Option<Expr>,
}

/// A function definition.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct Function {
    pub name: String,
    pub ret: Ty,
    pub params: Vec<(Ty, String)>,
    pub body: Vec<Stmt>,
}

/// A parsed translation unit.
#[derive(Clone, Debug, Default)]
#[allow(missing_docs)]
pub struct Unit {
    pub globals: Vec<Decl>,
    pub functions: Vec<Function>,
}

/// Statements.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub enum Stmt {
    Decl(Vec<Decl>),
    Expr(Expr),
    Return(Option<Expr>),
    If {
        cond: Expr,
        then: Vec<Stmt>,
        otherwise: Option<Vec<Stmt>>,
    },
    /// `for (init; cond; step) body` — unrolled at evaluation with a hard
    /// iteration cap, so a non-constant bound is a structured runtime error
    /// rather than an unbounded loop.
    For {
        init: Vec<Stmt>,
        cond: Option<Expr>,
        step: Option<Expr>,
        body: Vec<Stmt>,
    },
    Discard,
    Empty,
}

/// Binary operators.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(missing_docs)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// Expressions.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub enum Expr {
    Float(f32),
    Int(i32),
    Bool(bool),
    Ident(String),
    /// Constructor call (`vec4(..)`, `mat4(..)`) or builtin (`abs(..)`).
    Call(String, Vec<Expr>),
    Swizzle(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    /// `target op= value`, desugared at parse time into an assignment node.
    Assign {
        target: Box<Expr>,
        op: Option<BinOp>,
        value: Box<Expr>,
    },
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Float(f32),
    Int(i32),
    Punct(&'static str),
}

/// Tokenise, stripping comments, preprocessor lines and `precision` statements.
fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let bytes: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == '/' {
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == '*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == '*' && bytes[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        if c == '#' {
            // A directive line: a plain `#version`/`#extension`/`#line` is
            // harmless, but a conditional branch changes the meaning of the
            // program, which this interpreter cannot honour. Reject it.
            let start = i;
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
            let line: String = bytes[start..i].iter().collect();
            let directive = line.trim_start_matches('#').trim();
            let keyword = directive.split_whitespace().next().unwrap_or("");
            if !matches!(
                keyword,
                "version" | "extension" | "line" | "pragma" | "define" | "undef"
            ) {
                return Err(format!(
                    "preprocessor directive {line:?} is outside the interpreted GLSL subset"
                ));
            }
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == '_') {
                i += 1;
            }
            let word: String = bytes[start..i].iter().collect();
            if word == "precision" {
                // `precision mediump float;` — consume to the semicolon.
                while i < bytes.len() && bytes[i] != ';' {
                    i += 1;
                }
                i = (i + 1).min(bytes.len());
                continue;
            }
            out.push(Tok::Ident(word));
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit()) {
            let start = i;
            let mut is_float = false;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit()
                    || bytes[i] == '.'
                    || bytes[i] == 'e'
                    || bytes[i] == 'E'
                    || ((bytes[i] == '+' || bytes[i] == '-')
                        && i > start
                        && (bytes[i - 1] == 'e' || bytes[i - 1] == 'E')))
            {
                if bytes[i] == '.' || bytes[i] == 'e' || bytes[i] == 'E' {
                    is_float = true;
                }
                i += 1;
            }
            let text: String = bytes[start..i].iter().collect();
            let clean = text.trim_end_matches(['f', 'F']);
            if is_float {
                let v: f32 = clean
                    .parse()
                    .map_err(|_| format!("malformed float literal {text:?}"))?;
                out.push(Tok::Float(v));
            } else {
                let v: i32 = clean
                    .parse()
                    .map_err(|_| format!("malformed integer literal {text:?}"))?;
                out.push(Tok::Int(v));
            }
            continue;
        }
        const TWO: [&str; 10] = ["==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "++", "--"];
        if i + 1 < bytes.len() {
            let pair: String = bytes[i..i + 2].iter().collect();
            if let Some(op) = TWO.iter().find(|o| **o == pair) {
                out.push(Tok::Punct(op));
                i += 2;
                continue;
            }
        }
        let one: &'static str = match c {
            '+' => "+",
            '-' => "-",
            '*' => "*",
            '/' => "/",
            '(' => "(",
            ')' => ")",
            '{' => "{",
            '}' => "}",
            '[' => "[",
            ']' => "]",
            ',' => ",",
            ';' => ";",
            '.' => ".",
            '=' => "=",
            '<' => "<",
            '>' => ">",
            '!' => "!",
            ':' => ":",
            '?' => "?",
            _ => return Err(format!("unexpected character {c:?} in shader source")),
        };
        out.push(Tok::Punct(one));
        i += 1;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Punct(q)) if *q == p) {
            self.pos += 1;
            return true;
        }
        false
    }

    fn expect_punct(&mut self, p: &str) -> Result<(), String> {
        if self.eat_punct(p) {
            Ok(())
        } else {
            Err(format!("expected {p:?}, found {:?}", self.peek()))
        }
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        match self.next() {
            Some(Tok::Ident(s)) => Ok(s),
            other => Err(format!("expected an identifier, found {other:?}")),
        }
    }

    fn unit(&mut self) -> Result<Unit, String> {
        let mut unit = Unit::default();
        while self.peek().is_some() {
            if self.eat_punct(";") {
                continue;
            }
            // Look ahead for `ret name (` (a function) or a declaration.
            let start = self.pos;
            let _qual = self.qualifier();
            let ty_name = match self.peek() {
                Some(Tok::Ident(s)) => s.clone(),
                other => return Err(format!("expected a type, found {other:?}")),
            };
            let ty = match Ty::parse(&ty_name) {
                Some(t) => t,
                None => {
                    return Err(format!(
                        "unknown type {ty_name:?} (supported: void/bool/int/float/vec2-4/mat2-4/sampler2D)"
                    ))
                }
            };
            self.next();
            let name = self.expect_ident()?;
            if self.eat_punct("(") {
                let mut params = Vec::new();
                if !self.eat_punct(")") {
                    loop {
                        let _pqual = self.qualifier(); // `in`/`const` on params
                        let pty_name = self.expect_ident()?;
                        let pty = Ty::parse(&pty_name)
                            .ok_or_else(|| format!("unknown parameter type {pty_name:?}"))?;
                        let pname = self.expect_ident()?;
                        params.push((pty, pname));
                        if self.eat_punct(",") {
                            continue;
                        }
                        self.expect_punct(")")?;
                        break;
                    }
                }
                let body = self.block()?;
                unit.functions.push(Function {
                    name,
                    ret: ty,
                    params,
                    body,
                });
                continue;
            }
            // A global declaration list starting at `name`.
            self.pos = start;
            let decls = self.declaration()?;
            unit.globals.extend(decls);
        }
        Ok(unit)
    }

    fn qualifier(&mut self) -> Qual {
        match self.peek() {
            Some(Tok::Ident(s)) => {
                let q = match s.as_str() {
                    "attribute" => Qual::Attribute,
                    "uniform" => Qual::Uniform,
                    "varying" => Qual::Varying,
                    "const" => Qual::Const,
                    "in" | "out" | "inout" | "highp" | "mediump" | "lowp" => {
                        self.pos += 1;
                        return self.qualifier();
                    }
                    _ => return Qual::Plain,
                };
                self.pos += 1;
                q
            }
            _ => Qual::Plain,
        }
    }

    /// `qual type name (= expr)? (, name (= expr)?)* ;`
    fn declaration(&mut self) -> Result<Vec<Decl>, String> {
        let qual = self.qualifier();
        let ty_name = self.expect_ident()?;
        let ty = Ty::parse(&ty_name).ok_or_else(|| format!("unknown type {ty_name:?}"))?;
        let mut out = Vec::new();
        loop {
            let name = self.expect_ident()?;
            let mut init = None;
            if self.eat_punct("=") {
                init = Some(self.expr()?);
            }
            out.push(Decl {
                qual,
                ty,
                name,
                init,
            });
            if self.eat_punct(",") {
                continue;
            }
            self.expect_punct(";")?;
            return Ok(out);
        }
    }

    fn block(&mut self) -> Result<Vec<Stmt>, String> {
        self.expect_punct("{")?;
        let mut out = Vec::new();
        while !self.eat_punct("}") {
            if self.peek().is_none() {
                return Err("unbalanced braces: missing '}'".into());
            }
            out.push(self.stmt()?);
        }
        Ok(out)
    }

    fn stmt(&mut self) -> Result<Stmt, String> {
        if self.eat_punct(";") {
            return Ok(Stmt::Empty);
        }
        if matches!(self.peek(), Some(Tok::Punct("{"))) {
            return Ok(Stmt::Decl(Vec::new())).and_then(|_| {
                // Nested block: flatten into a synthetic `if (true)` body is
                // overkill; `Stmt` has no block variant, so wrap it.
                Ok(Stmt::If {
                    cond: Expr::Bool(true),
                    then: self.block()?,
                    otherwise: None,
                })
            });
        }
        if let Some(Tok::Ident(word)) = self.peek().cloned() {
            match word.as_str() {
                "if" => {
                    self.next();
                    self.expect_punct("(")?;
                    let cond = self.expr()?;
                    self.expect_punct(")")?;
                    let then = if matches!(self.peek(), Some(Tok::Punct("{"))) {
                        self.block()?
                    } else {
                        vec![self.stmt()?]
                    };
                    let otherwise = if matches!(self.peek(), Some(Tok::Ident(w)) if w == "else") {
                        self.next();
                        Some(if matches!(self.peek(), Some(Tok::Punct("{"))) {
                            self.block()?
                        } else {
                            vec![self.stmt()?]
                        })
                    } else {
                        None
                    };
                    return Ok(Stmt::If {
                        cond,
                        then,
                        otherwise,
                    });
                }
                "return" => {
                    self.next();
                    if self.eat_punct(";") {
                        return Ok(Stmt::Return(None));
                    }
                    let e = self.expr()?;
                    self.expect_punct(";")?;
                    return Ok(Stmt::Return(Some(e)));
                }
                "discard" => {
                    self.next();
                    self.expect_punct(";")?;
                    return Ok(Stmt::Discard);
                }
                "for" => {
                    self.next();
                    self.expect_punct("(")?;
                    let init = if self.eat_punct(";") {
                        Vec::new()
                    } else {
                        vec![self.stmt()?]
                    };
                    let cond = if self.eat_punct(";") {
                        None
                    } else {
                        let e = self.expr()?;
                        self.expect_punct(";")?;
                        Some(e)
                    };
                    let step = if self.eat_punct(")") {
                        None
                    } else {
                        let e = self.expr()?;
                        self.expect_punct(")")?;
                        Some(e)
                    };
                    let body = if matches!(self.peek(), Some(Tok::Punct("{"))) {
                        self.block()?
                    } else {
                        vec![self.stmt()?]
                    };
                    return Ok(Stmt::For {
                        init,
                        cond,
                        step,
                        body,
                    });
                }
                "while" | "do" => {
                    return Err(format!(
                        "`{word}` loops are outside the interpreted GLSL subset (only bounded \
                         `for` loops are evaluated)"
                    ))
                }
                "break" | "continue" => {
                    return Err(format!("`{word}` is outside the interpreted GLSL subset"))
                }
                "const" | "attribute" | "uniform" | "varying" | "highp" | "mediump" | "lowp" => {
                    return Ok(Stmt::Decl(self.declaration()?));
                }
                _ => {}
            }
            if Ty::parse(&word).is_some() {
                return Ok(Stmt::Decl(self.declaration()?));
            }
        }
        let e = self.expr()?;
        self.expect_punct(";")?;
        Ok(Stmt::Expr(e))
    }

    fn expr(&mut self) -> Result<Expr, String> {
        self.assignment()
    }

    fn assignment(&mut self) -> Result<Expr, String> {
        let lhs = self.binary(0)?;
        // Postfix `++`/`--` desugar to `target += 1.0`. `grab` must be a
        // plain or swizzled lvalue; the evaluator rejects anything else.
        let postfix_op = match self.peek() {
            Some(Tok::Punct("++")) => Some(BinOp::Add),
            Some(Tok::Punct("--")) => Some(BinOp::Sub),
            _ => None,
        };
        if let Some(op) = postfix_op {
            self.next();
            return Ok(Expr::Assign {
                target: Box::new(lhs),
                op: Some(op),
                value: Box::new(Expr::Float(1.0)),
            });
        }
        let op = match self.peek() {
            Some(Tok::Punct("=")) => None,
            Some(Tok::Punct("+=")) => Some(BinOp::Add),
            Some(Tok::Punct("-=")) => Some(BinOp::Sub),
            _ => return Ok(lhs),
        };
        self.next();
        let value = self.assignment()?;
        Ok(Expr::Assign {
            target: Box::new(lhs),
            op,
            value: Box::new(value),
        })
    }

    fn binary(&mut self, level: usize) -> Result<Expr, String> {
        // Lowest → highest precedence.
        const LEVELS: [&[(&str, BinOp)]; 6] = [
            &[("||", BinOp::Or)],
            &[("&&", BinOp::And)],
            &[("==", BinOp::Eq), ("!=", BinOp::Ne)],
            &[
                ("<", BinOp::Lt),
                ("<=", BinOp::Le),
                (">", BinOp::Gt),
                (">=", BinOp::Ge),
            ],
            &[("+", BinOp::Add), ("-", BinOp::Sub)],
            &[("*", BinOp::Mul), ("/", BinOp::Div)],
        ];
        if level >= LEVELS.len() {
            return self.unary();
        }
        let mut lhs = self.binary(level + 1)?;
        loop {
            let found = match self.peek() {
                Some(Tok::Punct(p)) => LEVELS[level].iter().find(|(op, _)| op == p).copied(),
                _ => None,
            };
            let Some((_, op)) = found else { return Ok(lhs) };
            self.next();
            let rhs = self.binary(level + 1)?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
    }

    fn unary(&mut self) -> Result<Expr, String> {
        let prefix_op = if self.eat_punct("++") {
            Some(BinOp::Add)
        } else if self.eat_punct("--") {
            Some(BinOp::Sub)
        } else {
            None
        };
        if let Some(op) = prefix_op {
            let target = self.unary()?;
            return Ok(Expr::Assign {
                target: Box::new(target),
                op: Some(op),
                value: Box::new(Expr::Float(1.0)),
            });
        }
        if self.eat_punct("-") {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.eat_punct("!") {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        if self.eat_punct("+") {
            return self.unary();
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.primary()?;
        loop {
            if self.eat_punct(".") {
                let field = self.expect_ident()?;
                e = Expr::Swizzle(Box::new(e), field);
                continue;
            }
            if self.eat_punct("[") {
                let idx = self.expr()?;
                self.expect_punct("]")?;
                e = Expr::Index(Box::new(e), Box::new(idx));
                continue;
            }
            return Ok(e);
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Float(v)) => Ok(Expr::Float(v)),
            Some(Tok::Int(v)) => Ok(Expr::Int(v)),
            Some(Tok::Ident(name)) => {
                if name == "true" {
                    return Ok(Expr::Bool(true));
                }
                if name == "false" {
                    return Ok(Expr::Bool(false));
                }
                if self.eat_punct("(") {
                    if matches!(name.as_str(), "dFdx" | "dFdy" | "fwidth") {
                        return Err(format!(
                            "`{name}` (fragment derivative builtin) is outside the interpreted \
                             GLSL subset"
                        ));
                    }
                    let mut args = Vec::new();
                    if !self.eat_punct(")") {
                        loop {
                            args.push(self.expr()?);
                            if self.eat_punct(",") {
                                continue;
                            }
                            self.expect_punct(")")?;
                            break;
                        }
                    }
                    return Ok(Expr::Call(name, args));
                }
                Ok(Expr::Ident(name))
            }
            Some(Tok::Punct("(")) => {
                let e = self.expr()?;
                self.expect_punct(")")?;
                Ok(e)
            }
            other => Err(format!("unexpected token {other:?}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Public compile entry points
// ---------------------------------------------------------------------------

/// Parse a shader source into a unit.
///
/// The stage determines which built-in names are meaningful, not the grammar.
pub fn parse(src: &str, stage: Stage) -> Result<Unit, String> {
    let _ = stage;
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0 };
    let unit = p.unit()?;
    if unit.functions.is_empty() {
        return Err("shader has no function definitions (no `void main()`)".into());
    }
    if !unit.functions.iter().any(|f| f.name == "main") {
        return Err("shader has no `main()`".into());
    }
    Ok(unit)
}

/// Interface of one stage: which globals the engine binds.
#[derive(Clone, Debug, Default)]
#[allow(missing_docs)]
pub struct Interface {
    pub attributes: Vec<String>,
    pub uniforms: Vec<String>,
    pub varyings: Vec<String>,
}

impl Unit {
    /// Report the globals declared by the shader.
    #[must_use]
    pub fn interface(&self) -> Interface {
        let mut i = Interface::default();
        for d in &self.globals {
            match d.qual {
                Qual::Attribute => i.attributes.push(d.name.clone()),
                Qual::Uniform => i.uniforms.push(d.name.clone()),
                Qual::Varying => i.varyings.push(d.name.clone()),
                _ => {}
            }
        }
        i
    }
}

// ---------------------------------------------------------------------------
// Evaluator
// ---------------------------------------------------------------------------

/// Texture access for `texture2D` during fragment evaluation.
pub trait TextureLookup {
    /// Sample `unit` at `(u, v)` in normalised `[0,1]` coordinates, returning
    /// straight (non-premultiplied) RGBA at the base level.
    fn sample(&self, unit: i32, u: f32, v: f32) -> [f32; 4];

    /// Sample with a LOD hint (in texels per pixel units). The default ignores
    /// the hint, so a level-0-only provider stays correct.
    fn sample_lod(&self, unit: i32, u: f32, v: f32, lod: f32) -> [f32; 4] {
        let _ = lod;
        self.sample(unit, u, v)
    }
}

/// A no-texture lookup (all samples are opaque black with alpha 1).
pub struct NoTextures;

impl TextureLookup for NoTextures {
    fn sample(&self, _unit: i32, _u: f32, _v: f32) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// One vertex shader invocation's output.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct VertexOut {
    /// Clip-space position.
    pub clip: [f32; 4],
    /// `gl_PointSize`.
    pub point_size: f32,
    /// Varying outputs in declaration order.
    pub varyings: Vec<Value>,
}

struct Scope {
    vars: std::collections::HashMap<String, Value>,
}

struct Env {
    scopes: Vec<Scope>,
    /// `gl_Position`/`gl_FragColor` style built-ins written by the shader.
    builtins: std::collections::HashMap<String, Value>,
}

impl Env {
    fn new() -> Env {
        Env {
            scopes: vec![Scope {
                vars: std::collections::HashMap::new(),
            }],
            builtins: std::collections::HashMap::new(),
        }
    }

    fn get(&self, name: &str) -> Option<Value> {
        for s in self.scopes.iter().rev() {
            if let Some(v) = s.vars.get(name) {
                return Some(v.clone());
            }
        }
        self.builtins.get(name).cloned()
    }

    fn set(&mut self, name: &str, v: Value) {
        for s in self.scopes.iter_mut().rev() {
            if let Some(slot) = s.vars.get_mut(name) {
                *slot = v;
                return;
            }
        }
        self.builtins.insert(name.to_string(), v);
    }

    fn declare(&mut self, name: &str, v: Value) {
        if let Some(s) = self.scopes.last_mut() {
            s.vars.insert(name.to_string(), v);
        }
    }

    fn push(&mut self) {
        self.scopes.push(Scope {
            vars: std::collections::HashMap::new(),
        });
    }

    fn pop(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }
}

/// Control-flow outcome of a statement list.
enum Flow {
    Normal,
    /// A `return`; the value is only meaningful for a value-returning function
    /// (GLSL ES 1.00 `main` returns void, so it is unused in practice).
    #[allow(dead_code)]
    Return(Option<Value>),
    Discard,
}

struct Interpreter<'a> {
    /// The translation unit, so user-defined calls can be dispatched.
    unit: &'a Unit,
    /// Texture units visible to `texture2D`.
    textures: &'a dyn TextureLookup,
    /// LOD hint forwarded to mipmapped texture lookups.
    lod: f32,
    /// Remaining AST evaluations for this invocation (see
    /// [`SHADER_STEP_BUDGET`]). Shared across the whole `run_body`/`run_stmt`
    /// recursion, which is what makes nested loops non-exponential.
    budget: std::cell::Cell<u32>,
    /// Current user-function call depth (see [`MAX_CALL_DEPTH`]).
    depth: std::cell::Cell<u32>,
}

impl<'a> Interpreter<'a> {
    /// Consume one unit of the per-invocation instruction budget.
    fn step(&self) -> EvalResult<()> {
        let left = self.budget.get();
        if left == 0 {
            return err(format!(
                "shader exceeded the {SHADER_STEP_BUDGET}-evaluation instruction budget; \
                 loops must be bounded (nested/unbounded loops are rejected at draw time)"
            ));
        }
        self.budget.set(left - 1);
        Ok(())
    }

    fn eval(&self, e: &Expr, env: &mut Env) -> EvalResult<Value> {
        self.step()?;
        match e {
            Expr::Float(v) => Ok(Value::F(*v)),
            Expr::Int(v) => Ok(Value::I(*v)),
            Expr::Bool(v) => Ok(Value::B(*v)),
            Expr::Ident(name) => env
                .get(name)
                .ok_or_else(|| EvalError(format!("unbound identifier {name:?}"))),
            Expr::Swizzle(base, field) => {
                let v = self.eval(base, env)?;
                let src = v.to_vec4();
                let n = match v {
                    Value::V(_, n) => n,
                    _ => 1,
                };
                let mut out = [0.0f32; 4];
                for (i, ch) in field.chars().enumerate() {
                    if i >= 4 {
                        return err(format!("swizzle {field:?} is longer than 4 components"));
                    }
                    let idx = match ch {
                        'x' | 'r' | 's' => 0,
                        'y' | 'g' | 't' => 1,
                        'z' | 'b' | 'p' => 2,
                        'w' | 'a' | 'q' => 3,
                        _ => return err(format!("bad swizzle component {ch:?}")),
                    };
                    if idx >= n.max(1) {
                        return err(format!("swizzle {field:?} reads past a {n}-component value"));
                    }
                    out[i] = src[idx];
                }
                let len = field.chars().count();
                if len == 1 {
                    Ok(Value::F(out[0]))
                } else {
                    Ok(Value::V(out, len))
                }
            }
            Expr::Index(base, idx) => {
                let v = self.eval(base, env)?;
                let i = self.eval(idx, env)?.to_f32() as usize;
                match v {
                    Value::V(c, n) => {
                        if i >= n {
                            return err(format!("vector index {i} out of range ({n})"));
                        }
                        Ok(Value::F(c[i]))
                    }
                    Value::M(m, n) => {
                        let col = i / n;
                        let row = i % n;
                        if col >= n || row >= n {
                            return err(format!("matrix index {i} out of range ({n}x{n})"));
                        }
                        Ok(Value::F(m[col * n + row]))
                    }
                    other => err(format!("cannot index a {}-wide value", other.width())),
                }
            }
            Expr::Neg(inner) => {
                let v = self.eval(inner, env)?;
                Ok(map1(&v, |x| -x))
            }
            Expr::Not(inner) => {
                let v = self.eval(inner, env)?;
                Ok(Value::B(!v.truthy()))
            }
            Expr::Bin(op, a, b) => {
                let lhs = self.eval(a, env)?;
                let rhs = self.eval(b, env)?;
                binop(*op, &lhs, &rhs)
            }
            Expr::Assign { target, op, value } => {
                let mut v = self.eval(value, env)?;
                if let Some(op) = op {
                    let current = self.eval(target, env)?;
                    v = binop(*op, &current, &v)?;
                }
                self.assign(target, v.clone(), env)?;
                Ok(v)
            }
            Expr::Call(name, args) => self.call(name, args, env),
        }
    }

    fn assign(&self, target: &Expr, v: Value, env: &mut Env) -> EvalResult<()> {
        match target {
            Expr::Ident(name) => {
                if env.get(name).is_some() {
                    env.set(name, v);
                    Ok(())
                } else {
                    err(format!("assignment to undeclared {name:?}"))
                }
            }
            Expr::Swizzle(base, field) => {
                let Expr::Ident(name) = &**base else {
                    return err("swizzle assignment requires a plain variable target");
                };
                let mut cur = env
                    .get(name)
                    .ok_or_else(|| EvalError(format!("unbound identifier {name:?}")))?;
                let Value::V(ref mut comps, n) = cur else {
                    return err("swizzle assignment requires a vector target");
                };
                let rhs = v.to_vec4();
                for (i, ch) in field.chars().enumerate() {
                    let idx = match ch {
                        'x' | 'r' | 's' => 0,
                        'y' | 'g' | 't' => 1,
                        'z' | 'b' | 'p' => 2,
                        'w' | 'a' | 'q' => 3,
                        _ => return err(format!("bad swizzle component {ch:?}")),
                    };
                    if idx >= n {
                        return err(format!("swizzle {field:?} writes past a {n}-component value"));
                    }
                    comps[idx] = rhs[i.min(3)];
                }
                env.set(name, cur);
                Ok(())
            }
            Expr::Index(base, idx) => {
                let Expr::Ident(name) = &**base else {
                    return err("indexed assignment requires a plain variable target");
                };
                let i = self.eval(idx, env)?.to_f32() as usize;
                let mut cur = env
                    .get(name)
                    .ok_or_else(|| EvalError(format!("unbound identifier {name:?}")))?;
                match &mut cur {
                    Value::V(comps, n) => {
                        if i >= *n {
                            return err(format!("vector index {i} out of range"));
                        }
                        comps[i] = v.to_f32();
                    }
                    Value::M(m, n) => {
                        if i >= *n * *n {
                            return err(format!("matrix index {i} out of range"));
                        }
                        m[i] = v.to_f32();
                    }
                    _ => return err("indexed assignment requires a vector or matrix"),
                }
                env.set(name, cur);
                Ok(())
            }
            _ => err("unsupported assignment target"),
        }
    }

    fn call(&self, name: &str, args: &[Expr], env: &mut Env) -> EvalResult<Value> {
        if let Some(ty) = Ty::parse(name) {
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                vals.push(self.eval(a, env)?);
            }
            return construct(ty, &vals);
        }
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            vals.push(self.eval(a, env)?);
        }
        // A user-defined function with this name/arity shadows a builtin of the
        // same signature. Parameters get their own scope; assignments to
        // globals still reach the enclosing scopes (GLSL globals are mutable).
        if let Some(f) = self
            .unit
            .functions
            .iter()
            .find(|f| f.name == name && f.params.len() == vals.len())
        {
            let depth = self.depth.get();
            if depth >= MAX_CALL_DEPTH {
                return err(format!(
                    "shader exceeded the maximum function call depth ({MAX_CALL_DEPTH}); \
                     recursion is not supported"
                ));
            }
            self.depth.set(depth + 1);
            env.push();
            for ((_, pname), v) in f.params.iter().zip(vals) {
                env.declare(pname, v);
            }
            let flow = self.run_body(&f.body, env);
            env.pop();
            self.depth.set(depth);
            return match flow? {
                Flow::Return(Some(v)) => Ok(v),
                Flow::Return(None) | Flow::Normal => Ok(zero_of(f.ret)),
                Flow::Discard => err("`discard` is only legal in main()"),
            };
        }
        builtin(name, &vals, self.textures, self.lod)
    }

    fn run_body(&self, body: &[Stmt], env: &mut Env) -> EvalResult<Flow> {
        for s in body {
            match self.run_stmt(s, env)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn run_stmt(&self, s: &Stmt, env: &mut Env) -> EvalResult<Flow> {
        self.step()?;
        match s {
            Stmt::Empty => Ok(Flow::Normal),
            Stmt::Decl(decls) => {
                for d in decls {
                    let v = match &d.init {
                        Some(e) => self.eval(e, env)?,
                        None => zero_of(d.ty),
                    };
                    env.declare(&d.name, v);
                }
                Ok(Flow::Normal)
            }
            Stmt::Expr(e) => {
                self.eval(e, env)?;
                Ok(Flow::Normal)
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => Some(self.eval(e, env)?),
                    None => None,
                };
                Ok(Flow::Return(v))
            }
            Stmt::Discard => Ok(Flow::Discard),
            Stmt::If {
                cond,
                then,
                otherwise,
            } => {
                let c = self.eval(cond, env)?.truthy();
                env.push();
                let flow = if c {
                    self.run_body(then, env)
                } else if let Some(otherwise) = otherwise {
                    self.run_body(otherwise, env)
                } else {
                    Ok(Flow::Normal)
                };
                env.pop();
                flow
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                env.push();
                let mut flow = Flow::Normal;
                for s in init {
                    match self.run_stmt(s, env)? {
                        Flow::Normal => {}
                        other => {
                            flow = other;
                            break;
                        }
                    }
                }
                let mut iterations = 0u32;
                loop {
                    if !matches!(flow, Flow::Normal) {
                        break;
                    }
                    if let Some(cond) = cond {
                        if !self.eval(cond, env)?.truthy() {
                            break;
                        }
                    }
                    flow = self.run_body(body, env)?;
                    if let Some(step) = step {
                        self.eval(step, env)?;
                    }
                    iterations += 1;
                    if iterations > FOR_ITERATION_CAP {
                        env.pop();
                        return err(format!(
                            "for loop exceeded the interpreter iteration cap ({FOR_ITERATION_CAP}); \
                             only bounded loops are evaluated"
                        ));
                    }
                }
                env.pop();
                Ok(flow)
            }
        }
    }
}

fn zero_of(ty: Ty) -> Value {
    match ty {
        Ty::Void => Value::F(0.0),
        Ty::Bool => Value::B(false),
        Ty::Int => Value::I(0),
        Ty::Float => Value::F(0.0),
        Ty::Vec2 => Value::V([0.0; 4], 2),
        Ty::Vec3 => Value::V([0.0; 4], 3),
        Ty::Vec4 => Value::V([0.0; 4], 4),
        Ty::Mat2 => Value::M([0.0; 16], 2),
        Ty::Mat3 => Value::M([0.0; 16], 3),
        Ty::Mat4 => Value::M([0.0; 16], 4),
        Ty::Sampler2D => Value::S(0),
    }
}

fn map1(v: &Value, f: impl Fn(f32) -> f32) -> Value {
    match v {
        Value::F(x) => Value::F(f(*x)),
        Value::I(x) => Value::F(f(*x as f32)),
        Value::B(b) => Value::F(f(f32::from(u8::from(*b)))),
        Value::V(c, n) => {
            let mut out = *c;
            for slot in out.iter_mut().take(*n) {
                *slot = f(*slot);
            }
            Value::V(out, *n)
        }
        Value::M(m, n) => {
            let mut out = *m;
            for slot in out.iter_mut().take(n * n) {
                *slot = f(*slot);
            }
            Value::M(out, *n)
        }
        Value::S(s) => Value::F(f(*s as f32)),
    }
}

fn map2(a: &Value, b: &Value, f: impl Fn(f32, f32) -> f32) -> Value {
    match (a, b) {
        (Value::V(ca, n), _) => {
            let cb = b.to_vec4();
            let mut out = [0.0f32; 4];
            for i in 0..*n {
                out[i] = f(ca[i], cb[i]);
            }
            Value::V(out, *n)
        }
        (_, Value::V(cb, n)) => {
            let ca = a.to_vec4();
            let mut out = [0.0f32; 4];
            for i in 0..*n {
                out[i] = f(ca[i], cb[i]);
            }
            Value::V(out, *n)
        }
        _ => Value::F(f(a.to_f32(), b.to_f32())),
    }
}

fn binop(op: BinOp, a: &Value, b: &Value) -> EvalResult<Value> {
    match op {
        BinOp::And => return Ok(Value::B(a.truthy() && b.truthy())),
        BinOp::Or => return Ok(Value::B(a.truthy() || b.truthy())),
        BinOp::Lt => return Ok(Value::B(a.to_f32() < b.to_f32())),
        BinOp::Le => return Ok(Value::B(a.to_f32() <= b.to_f32())),
        BinOp::Gt => return Ok(Value::B(a.to_f32() > b.to_f32())),
        BinOp::Ge => return Ok(Value::B(a.to_f32() >= b.to_f32())),
        BinOp::Eq => return Ok(Value::B(components_eq(a, b))),
        BinOp::Ne => return Ok(Value::B(!components_eq(a, b))),
        _ => {}
    }
    if op == BinOp::Mul {
        match (a, b) {
            (Value::M(m, n), Value::V(v, vn)) => {
                // Column-major storage: result = sum over columns of col_i * v_i.
                let mut out = [0.0f32; 4];
                for r in 0..*n {
                    let mut acc = 0.0;
                    for c in 0..*n {
                        let vc = if c < *vn { v[c] } else { 0.0 };
                        acc += m[c * n + r] * vc;
                    }
                    out[r] = acc;
                }
                return Ok(Value::V(out, *n));
            }
            (Value::V(v, vn), Value::M(m, n)) => {
                let mut out = [0.0f32; 4];
                for c in 0..*n {
                    let mut acc = 0.0;
                    for r in 0..*n {
                        let vr = if r < *vn { v[r] } else { 0.0 };
                        acc += m[c * n + r] * vr;
                    }
                    out[c] = acc;
                }
                return Ok(Value::V(out, *n));
            }
            (Value::M(ma, na), Value::M(mb, nb)) if na == nb => {
                let n = *na;
                let mut out = [0.0f32; 16];
                for c in 0..n {
                    for r in 0..n {
                        let mut acc = 0.0;
                        for k in 0..n {
                            acc += ma[k * n + r] * mb[c * n + k];
                        }
                        out[c * n + r] = acc;
                    }
                }
                return Ok(Value::M(out, n));
            }
            _ => {}
        }
    }
    Ok(match op {
        BinOp::Add => map2(a, b, |x, y| x + y),
        BinOp::Sub => map2(a, b, |x, y| x - y),
        BinOp::Mul => map2(a, b, |x, y| x * y),
        BinOp::Div => map2(a, b, |x, y| if y == 0.0 { 0.0 } else { x / y }),
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::And
        | BinOp::Or => unreachable!("handled above"),
    })
}

fn components_eq(a: &Value, b: &Value) -> bool {
    let (x, y) = (a.to_vec4(), b.to_vec4());
    let n = a.width().max(b.width()).min(4);
    (0..n).all(|i| (x[i] - y[i]).abs() < f32::EPSILON)
}

fn construct(ty: Ty, args: &[Value]) -> EvalResult<Value> {
    // Scalars contribute one component, vectors/matrices their live extent.
    // (Broadcasting a scalar to four here would corrupt `vec4(a, b, c, d)`.)
    let mut flat: Vec<f32> = Vec::new();
    for a in args {
        match a {
            Value::F(x) => flat.push(*x),
            Value::I(x) => flat.push(*x as f32),
            Value::B(b) => flat.push(f32::from(u8::from(*b))),
            Value::V(c, n) => flat.extend_from_slice(&c[..*n]),
            Value::M(m, n) => flat.extend_from_slice(&m[..n * n]),
            Value::S(s) => flat.push(*s as f32),
        }
    }
    let n = ty.cols();
    if ty.is_matrix() {
        if args.len() == 1 && args[0].width() == 1 {
            // `mat4(1.0)` → diagonal.
            let mut m = [0.0f32; 16];
            for i in 0..n {
                m[i * n + i] = args[0].to_f32();
            }
            return Ok(Value::M(m, n));
        }
        if flat.len() < n * n {
            return err(format!(
                "{} constructor needs {} components, got {}",
                ty_name(ty),
                n * n,
                flat.len()
            ));
        }
        let mut m = [0.0f32; 16];
        m[..n * n].copy_from_slice(&flat[..n * n]);
        return Ok(Value::M(m, n));
    }
    match ty {
        Ty::Float => Ok(Value::F(args.first().map_or(0.0, Value::to_f32))),
        Ty::Int => Ok(Value::I(args.first().map_or(0, |v| v.to_f32() as i32))),
        Ty::Bool => Ok(Value::B(args.first().is_some_and(Value::truthy))),
        Ty::Vec2 | Ty::Vec3 | Ty::Vec4 => {
            if args.len() == 1 && args[0].width() == 1 {
                // `vec4(x)` broadcasts a single scalar (GLSL ES 1.00 §5.4.2).
                return Ok(Value::V([args[0].to_f32(); 4], n));
            }
            if flat.len() < n {
                return err(format!(
                    "{} constructor needs {n} components, got {}",
                    ty_name(ty),
                    flat.len()
                ));
            }
            let mut out = [0.0f32; 4];
            out[..n].copy_from_slice(&flat[..n]);
            Ok(Value::V(out, n))
        }
        _ => err(format!("cannot construct {}", ty_name(ty))),
    }
}

fn ty_name(ty: Ty) -> &'static str {
    match ty {
        Ty::Void => "void",
        Ty::Bool => "bool",
        Ty::Int => "int",
        Ty::Float => "float",
        Ty::Vec2 => "vec2",
        Ty::Vec3 => "vec3",
        Ty::Vec4 => "vec4",
        Ty::Mat2 => "mat2",
        Ty::Mat3 => "mat3",
        Ty::Mat4 => "mat4",
        Ty::Sampler2D => "sampler2D",
    }
}

fn builtin(
    name: &str,
    args: &[Value],
    textures: &dyn TextureLookup,
    lod: f32,
) -> EvalResult<Value> {
    let a = |i: usize| args.get(i).cloned().unwrap_or(Value::F(0.0));
    Ok(match name {
        "texture2D" => {
            let unit = match a(0) {
                Value::S(u) => u,
                other => other.to_f32() as i32,
            };
            let c = a(1).to_vec4();
            let s = textures.sample_lod(unit, c[0], c[1], lod);
            Value::vec4(s[0], s[1], s[2], s[3])
        }
        "abs" => map1(&a(0), f32::abs),
        "floor" => map1(&a(0), f32::floor),
        "ceil" => map1(&a(0), f32::ceil),
        "fract" => map1(&a(0), |x| x - x.floor()),
        "sign" => map1(&a(0), f32::signum),
        "sqrt" => map1(&a(0), |x| x.max(0.0).sqrt()),
        "inversesqrt" => map1(&a(0), |x| if x > 0.0 { 1.0 / x.sqrt() } else { 0.0 }),
        "exp" => map1(&a(0), f32::exp),
        "log" => map1(&a(0), |x| if x > 0.0 { x.ln() } else { 0.0 }),
        "exp2" => map1(&a(0), f32::exp2),
        "log2" => map1(&a(0), |x| if x > 0.0 { x.log2() } else { 0.0 }),
        "sin" => map1(&a(0), f32::sin),
        "cos" => map1(&a(0), f32::cos),
        "tan" => map1(&a(0), f32::tan),
        "atan" => {
            if args.len() > 1 {
                map2(&a(0), &a(1), f32::atan2)
            } else {
                map1(&a(0), f32::atan)
            }
        }
        "radians" => map1(&a(0), f32::to_radians),
        "degrees" => map1(&a(0), f32::to_degrees),
        "mod" => map2(&a(0), &a(1), |x, y| if y == 0.0 { 0.0 } else { x - y * (x / y).floor() }),
        "min" => map2(&a(0), &a(1), f32::min),
        "max" => map2(&a(0), &a(1), f32::max),
        "pow" => map2(&a(0), &a(1), |x, y| x.powf(y)),
        "step" => map2(&a(1), &a(0), |x, edge| if x < edge { 0.0 } else { 1.0 }),
        "clamp" => {
            let (x, lo, hi) = (a(0), a(1), a(2));
            let hi = map2(&x, &hi, f32::min);
            map2(&hi, &lo, f32::max)
        }
        "mix" => {
            let (x, y, t) = (a(0), a(1), a(2));
            let d = map2(&y, &x, |y, x| y - x);
            let dt = map2(&d, &t, |d, t| d * t);
            map2(&x, &dt, |x, dt| x + dt)
        }
        "smoothstep" => {
            let (e0, e1, x) = (a(0), a(1), a(2));
            let t = {
                let d = map2(&x, &e0, |x, e0| x - e0);
                let r = map2(&e1, &e0, |e1, e0| e1 - e0);
                map2(&d, &r, |d, r| if r == 0.0 { 0.0 } else { d / r })
            };
            let clamped = {
                let hi = map2(&t, &Value::F(1.0), f32::min);
                map2(&hi, &Value::F(0.0), f32::max)
            };
            // t*t*(3-2t)
            let sq = map2(&clamped, &clamped, |x, y| x * y);
            let three = Value::F(3.0);
            let two = Value::F(2.0);
            let t2 = map2(&clamped, &two, |t, two| t * two);
            let inner = map2(&three, &t2, |three, t2| three - t2);
            map2(&sq, &inner, |x, y| x * y)
        }
        "length" => {
            let c = a(0).to_vec4();
            let n = a(0).width().min(4);
            Value::F(c[..n].iter().map(|x| x * x).sum::<f32>().sqrt())
        }
        "distance" => {
            let (x, y) = (a(0).to_vec4(), a(1).to_vec4());
            let n = a(0).width().max(a(1).width()).min(4);
            Value::F((0..n).map(|i| (x[i] - y[i]).powi(2)).sum::<f32>().sqrt())
        }
        "dot" => {
            let (x, y) = (a(0).to_vec4(), a(1).to_vec4());
            let n = a(0).width().max(a(1).width()).min(4);
            Value::F((0..n).map(|i| x[i] * y[i]).sum())
        }
        "cross" => {
            let (x, y) = (a(0).to_vec4(), a(1).to_vec4());
            Value::vec4(
                x[1] * y[2] - x[2] * y[1],
                x[2] * y[0] - x[0] * y[2],
                x[0] * y[1] - x[1] * y[0],
                0.0,
            )
        }
        "normalize" => {
            let v = a(0);
            let c = v.to_vec4();
            let n = v.width().min(4);
            let len = c[..n].iter().map(|x| x * x).sum::<f32>().sqrt();
            if len == 0.0 {
                v
            } else {
                let mut out = c;
                for slot in out.iter_mut().take(n) {
                    *slot /= len;
                }
                Value::V(out, n)
            }
        }
        "any" => Value::B(a(0).to_vec4().iter().any(|x| *x != 0.0)),
        "all" => Value::B(a(0).to_vec4().iter().all(|x| *x != 0.0)),
        "not" => Value::B(!a(0).truthy()),
        "matrixCompMult" => match (a(0), a(1)) {
            (Value::M(x, n), Value::M(y, m)) if n == m => {
                let mut out = [0.0f32; 16];
                for i in 0..n * n {
                    out[i] = x[i] * y[i];
                }
                Value::M(out, n)
            }
            _ => return err("matrixCompMult requires two equal-sized matrices"),
        },
        other => {
            return err(format!(
                "builtin `{other}` is outside the interpreted GLSL subset"
            ))
        }
    })
}

/// Evaluate the vertex stage.
pub fn run_vertex(
    unit: &Unit,
    uniforms: &[(String, Value)],
    attributes: &[(String, Value)],
    varyings: &[String],
) -> EvalResult<VertexOut> {
    let mut env = Env::new();
    for (name, v) in uniforms.iter().chain(attributes.iter()) {
        env.declare(name, v.clone());
    }
    for name in varyings {
        env.declare(name, zero_of(
            unit.globals
                .iter()
                .find(|d| &d.name == name)
                .map_or(Ty::Vec4, |d| d.ty),
        ));
    }
    env.declare("gl_Position", Value::vec4(0.0, 0.0, 0.0, 1.0));
    env.declare("gl_PointSize", Value::F(1.0));
    let interp = Interpreter {
        unit,
        textures: &NoTextures,
        lod: 0.0,
        budget: std::cell::Cell::new(SHADER_STEP_BUDGET),
        depth: std::cell::Cell::new(0),
    };
    let main = unit
        .functions
        .iter()
        .find(|f| f.name == "main")
        .ok_or_else(|| EvalError("shader has no main()".into()))?;
    match interp.run_body(&main.body, &mut env)? {
        Flow::Normal | Flow::Return(_) => {}
        Flow::Discard => return err("`discard` is only legal in a fragment shader"),
    }
    let clip = env
        .get("gl_Position")
        .map(|v| v.to_vec4())
        .ok_or_else(|| EvalError("vertex shader did not write gl_Position".into()))?;
    let point_size = env.get("gl_PointSize").map_or(1.0, |v| v.to_f32());
    let mut out = Vec::with_capacity(varyings.len());
    for name in varyings {
        // A varying the vertex stage never writes stays at its declared zero
        // value; the link step is what decides whether that is legal.
        out.push(env.get(name).unwrap_or_else(|| {
            zero_of(
                unit.globals
                    .iter()
                    .find(|d| &d.name == name)
                    .map_or(Ty::Vec4, |d| d.ty),
            )
        }));
    }
    Ok(VertexOut {
        clip,
        point_size,
        varyings: out,
    })
}

/// Evaluate the fragment stage. `Ok(None)` means `discard`.
pub fn run_fragment(
    unit: &Unit,
    uniforms: &[(String, Value)],
    varyings: &[String],
    interpolation: &[Value],
    frag_coords: [f32; 4],
    textures: &dyn TextureLookup,
) -> EvalResult<Option<[f32; 4]>> {
    run_fragment_lod(
        unit,
        uniforms,
        varyings,
        interpolation,
        frag_coords,
        0.0,
        textures,
    )
}

/// Like [`run_fragment`] but forwards a LOD hint to mipmapped texture lookups.
pub fn run_fragment_lod(
    unit: &Unit,
    uniforms: &[(String, Value)],
    varyings: &[String],
    interpolation: &[Value],
    frag_coords: [f32; 4],
    lod: f32,
    textures: &dyn TextureLookup,
) -> EvalResult<Option<[f32; 4]>> {
    let mut env = Env::new();
    for (name, v) in uniforms {
        env.declare(name, v.clone());
    }
    for (name, v) in varyings.iter().zip(interpolation.iter()) {
        env.declare(name, v.clone());
    }
    env.declare("gl_FragCoord", Value::V(frag_coords, 4));
    env.declare("gl_FrontFacing", Value::B(true));
    env.declare("gl_FragColor", Value::vec4(0.0, 0.0, 0.0, 1.0));
    env.declare("gl_FragData", Value::vec4(0.0, 0.0, 0.0, 1.0));
    let interp = Interpreter {
        unit,
        textures,
        lod,
        budget: std::cell::Cell::new(SHADER_STEP_BUDGET),
        depth: std::cell::Cell::new(0),
    };
    let main = unit
        .functions
        .iter()
        .find(|f| f.name == "main")
        .ok_or_else(|| EvalError("shader has no main()".into()))?;
    match interp.run_body(&main.body, &mut env)? {
        Flow::Discard => return Ok(None),
        Flow::Normal | Flow::Return(_) => {}
    }
    Ok(Some(env.get("gl_FragColor").map_or(
        [0.0, 0.0, 0.0, 1.0],
        |v| v.to_vec4(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VS: &str = r#"
        // trivial pass-through vertex shader
        #version 100
        precision mediump float;
        uniform mat4 u_mvp;
        attribute vec4 a_pos;
        attribute vec2 a_uv;
        varying vec2 v_uv;
        void main() {
            v_uv = a_uv;
            gl_Position = u_mvp * a_pos;
        }
    "#;

    const FS: &str = r#"
        precision mediump float;
        uniform sampler2D u_tex;
        uniform vec4 u_color;
        varying vec2 v_uv;
        void main() {
            vec4 t = texture2D(u_tex, v_uv);
            gl_FragColor = t * u_color;
        }
    "#;

    #[test]
    fn compiles_the_engine_style_shader_pair() {
        let vs = parse(VS, Stage::Vertex).expect("vertex shader accepted");
        let fs = parse(FS, Stage::Fragment).expect("fragment shader accepted");
        let vi = vs.interface();
        assert_eq!(vi.uniforms, vec!["u_mvp".to_string()]);
        assert_eq!(vi.attributes, vec!["a_pos".to_string(), "a_uv".to_string()]);
        assert_eq!(vi.varyings, vec!["v_uv".to_string()]);
        assert_eq!(fs.interface().varyings, vec!["v_uv".to_string()]);
    }

    #[test]
    fn rejects_outside_subset_with_a_reason() {
        let derivative = "void main() { float d = dFdx(gl_FragCoord.x); }";
        let e = parse(derivative, Stage::Fragment).unwrap_err();
        assert!(e.contains("dFdx"), "{e}");

        let while_loop = "void main() { while (true) {} }";
        let e = parse(while_loop, Stage::Vertex).unwrap_err();
        assert!(e.contains("`while`"), "{e}");

        let pp = "#ifdef GL_ES\nvoid main() {}\n#else\nvoid main() {}\n#endif";
        let e = parse(pp, Stage::Vertex).unwrap_err();
        assert!(e.contains("preprocessor"), "{e}");

        let missing_main = "attribute vec4 p;";
        let e = parse(missing_main, Stage::Vertex).unwrap_err();
        assert!(e.contains("main"), "{e}");
    }

    struct Solid([f32; 4]);

    impl TextureLookup for Solid {
        fn sample(&self, _unit: i32, _u: f32, _v: f32) -> [f32; 4] {
            self.0
        }
    }

    #[test]
    fn evaluates_the_vertex_stage_with_a_uniform_matrix() {
        let unit = parse(VS, Stage::Vertex).unwrap();
        // Column-major 2x translate in x, 1x scale.
        let mvp = [
            1.0, 0.0, 0.0, 0.0, // col 0
            0.0, 1.0, 0.0, 0.0, // col 1
            0.0, 0.0, 1.0, 0.0, // col 2
            0.5, 0.0, 0.0, 1.0, // col 3
        ];
        let out = run_vertex(
            &unit,
            &[("u_mvp".to_string(), Value::M(mvp, 4))],
            &[
                ("a_pos".to_string(), Value::vec4(0.5, -0.25, 0.0, 1.0)),
                ("a_uv".to_string(), Value::V([0.25, 0.75, 0.0, 0.0], 2)),
            ],
            &["v_uv".to_string()],
        )
        .unwrap();
        assert!((out.clip[0] - 1.0).abs() < 1e-6, "{:?}", out.clip);
        assert!((out.clip[1] + 0.25).abs() < 1e-6, "{:?}", out.clip);
        assert_eq!(out.varyings.len(), 1);
        assert_eq!(out.varyings[0].to_vec4()[0], 0.25);
    }

    #[test]
    fn evaluates_bounded_for_loops() {
        let src = "precision mediump float; void main() { float acc = 0.0; \
                   for (int i = 0; i < 4; i++) { acc += 1.0; } gl_FragColor = vec4(acc); }";
        let unit = parse(src, Stage::Fragment).unwrap();
        let out = run_fragment(&unit, &[], &[], &[], [0.0; 4], &NoTextures)
            .unwrap()
            .expect("not discarded");
        assert!((out[0] - 4.0).abs() < 1e-6, "{out:?}");
    }

    #[test]
    fn evaluates_the_fragment_stage_and_discards() {
        let unit = parse(FS, Stage::Fragment).unwrap();
        let uniforms = [
            ("u_color".to_string(), Value::vec4(0.5, 0.5, 0.5, 1.0)),
            ("u_tex".to_string(), Value::S(0)),
        ];
        let got = run_fragment(
            &unit,
            &uniforms,
            &["v_uv".to_string()],
            &[Value::V([0.0, 0.0, 0.0, 0.0], 2)],
            [0.0, 0.0, 0.0, 1.0],
            &Solid([0.2, 0.4, 0.6, 1.0]),
        )
        .unwrap()
        .expect("not discarded");
        assert!((got[0] - 0.1).abs() < 1e-6, "{got:?}");
        assert!((got[2] - 0.3).abs() < 1e-6, "{got:?}");

        let discarding = "precision mediump float; void main() { if (gl_FragCoord.x > 1.0) discard; gl_FragColor = vec4(1.0); }";
        let unit = parse(discarding, Stage::Fragment).unwrap();
        let out = run_fragment(
            &unit,
            &[],
            &[],
            &[],
            [2.0, 0.0, 0.0, 1.0],
            &NoTextures,
        )
        .unwrap();
        assert!(out.is_none(), "the fragment must be discarded");
    }

    #[test]
    fn nested_for_loops_are_bounded_by_the_global_budget() {
        // W94-A #4: the per-loop cap alone allows 4096^4 iterations here.
        // The per-invocation budget must stop it promptly with a structured
        // error instead of hanging the draw.
        let src = "precision mediump float; void main() { float s = 0.0; \
                   for (int a = 0; a < 4096; a++) \
                   for (int b = 0; b < 4096; b++) \
                   for (int c = 0; c < 4096; c++) \
                   for (int d = 0; d < 4096; d++) s += 0.0; \
                   gl_FragColor = vec4(s); }";
        let unit = parse(src, Stage::Fragment).expect("loops parse");
        let start = std::time::Instant::now();
        let err = run_fragment(&unit, &[], &[], &[], [0.0; 4], &NoTextures)
            .expect_err("hostile nested loops must be rejected");
        assert!(
            err.0.contains("instruction budget"),
            "expected the instruction-budget rejection, got {err}"
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "the budget must bound work, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn matrix_comp_mult_is_component_wise() {
        let a = Value::M(
            [
                1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0,
                15.0, 16.0,
            ],
            2,
        );
        let b = Value::M([2.0; 16], 2);
        let got = builtin("matrixCompMult", &[a, b], &NoTextures, 0.0).expect("comp mult");
        match got {
            Value::M(m, 2) => {
                assert_eq!(&m[..4], &[2.0, 4.0, 6.0, 8.0]);
            }
            other => panic!("expected a mat2, got {other:?}"),
        }
        // A non-matrix argument is rejected, never a silent placeholder.
        let e = builtin("matrixCompMult", &[Value::F(1.0), Value::F(2.0)], &NoTextures, 0.0)
            .expect_err("scalars must be rejected");
        assert!(e.0.contains("matrixCompMult"), "{e}");
    }

    #[test]
    fn user_defined_functions_are_dispatched() {
        let src = "precision mediump float; \
                   float twice(float x) { return x * 2.0; } \
                   float add(float a, float b) { return a + b; } \
                   void main() { gl_FragColor = vec4(add(twice(0.25), 0.5)); }";
        let unit = parse(src, Stage::Fragment).unwrap();
        let out = run_fragment(&unit, &[], &[], &[], [0.0; 4], &NoTextures)
            .unwrap()
            .expect("not discarded");
        assert!((out[0] - 1.0).abs() < 1e-6, "{out:?}");
    }

    #[test]
    fn runaway_recursion_hits_the_instruction_budget() {
        let src = "precision mediump float; float f(float x) { return f(x); } \
                   void main() { gl_FragColor = vec4(f(1.0)); }";
        let unit = parse(src, Stage::Fragment).unwrap();
        let e = run_fragment(&unit, &[], &[], &[], [0.0; 4], &NoTextures)
            .expect_err("unbounded recursion must be rejected");
        assert!(e.0.contains("call depth"), "{e}");
    }
}
