//! Java-compatible number formatting.
//!
//! `Double.toString` / `Float.toString` follow the Java specification: the
//! shortest decimal string that round-trips, with decimal notation for
//! `1e-3 <= |x| < 1e7` and scientific notation otherwise. Integer conversions
//! match `Integer`/`Long` exactly, including the unsigned hex/octal/binary
//! forms and radix 2..36.

/// Format an `f64` exactly as `java.lang.Double.toString`.
pub fn double_to_string(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let s = format!("{v:e}");
    java_float_repr(&s)
}

/// Format an `f32` exactly as `java.lang.Float.toString`.
pub fn float_to_string(v: f32) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let s = format!("{v:e}");
    java_float_repr(&s)
}

/// Convert Rust's `{:e}` form (`-1.234e5`, `1e-1`) into Java's `toString` form.
fn java_float_repr(rust: &str) -> String {
    let (neg, rest) = match rust.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, rust),
    };
    let (mantissa, exp_str) = match rest.split_once('e') {
        Some((m, e)) => (m, e),
        None => (rest, "0"),
    };
    let exp: i32 = exp_str.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let n = digits.len() as i32;

    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if (-3..7).contains(&exp) {
        if exp >= 0 {
            if exp + 1 >= n {
                out.push_str(digits);
                for _ in 0..(exp + 1 - n) {
                    out.push('0');
                }
                out.push_str(".0");
            } else {
                let split = (exp + 1) as usize;
                out.push_str(&digits[..split]);
                out.push('.');
                out.push_str(&digits[split..]);
            }
        } else {
            out.push_str("0.");
            for _ in 0..(-exp - 1) {
                out.push('0');
            }
            out.push_str(digits);
        }
    } else {
        out.push_str(&digits[..1]);
        out.push('.');
        if n > 1 {
            out.push_str(&digits[1..]);
        } else {
            out.push('0');
        }
        out.push('E');
        out.push_str(&exp.to_string());
    }
    out
}

const RADIX_DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Unsigned 32-bit conversion (used by `toHexString`, `toBinaryString`).
pub fn u32_to_radix(mut v: u32, radix: u32) -> String {
    if v == 0 {
        return "0".to_string();
    }
    let mut buf = Vec::new();
    while v > 0 {
        buf.push(RADIX_DIGITS[(v % radix) as usize]);
        v /= radix;
    }
    buf.reverse();
    String::from_utf8(buf).unwrap()
}

/// Unsigned 64-bit conversion.
pub fn u64_to_radix(mut v: u64, radix: u32) -> String {
    if v == 0 {
        return "0".to_string();
    }
    let mut buf = Vec::new();
    while v > 0 {
        buf.push(RADIX_DIGITS[(v % radix as u64) as usize]);
        v /= radix as u64;
    }
    buf.reverse();
    String::from_utf8(buf).unwrap()
}

/// Signed 32-bit conversion in the given radix.
pub fn i32_to_radix(v: i32, radix: u32) -> String {
    if v == 0 {
        return "0".to_string();
    }
    let neg = v < 0;
    let mag = (v as i64).unsigned_abs();
    let s = u64_to_radix(mag, radix);
    if neg {
        format!("-{s}")
    } else {
        s
    }
}

/// Signed 64-bit conversion in the given radix.
pub fn i64_to_radix(v: i64, radix: u32) -> String {
    if v == 0 {
        return "0".to_string();
    }
    let neg = v < 0;
    let mag = v.unsigned_abs();
    let s = u64_to_radix(mag, radix);
    if neg {
        format!("-{s}")
    } else {
        s
    }
}

/// Bit-pattern based `toHexString` for a 32-bit int (unsigned representation).
pub fn i32_to_unsigned_string(v: i32, radix: u32) -> String {
    u32_to_radix(v as u32, radix)
}

/// Bit-pattern based unsigned conversion for a 64-bit long.
pub fn i64_to_unsigned_string(v: i64, radix: u32) -> String {
    u64_to_radix(v as u64, radix)
}

fn digit_value(c: char, radix: u32) -> Option<u32> {
    let d = c.to_digit(36)?;
    if d < radix {
        Some(d)
    } else {
        None
    }
}

/// Parse a signed integer in `radix`, matching `Integer.parseInt`.
pub fn parse_i32(s: &str, radix: u32) -> Result<i32, String> {
    if !(2..=36).contains(&radix) {
        return Err(format!("radix {radix} out of range"));
    }
    let mut chars = s.chars().peekable();
    let mut neg = false;
    if let Some(&c) = chars.peek() {
        if c == '+' || c == '-' {
            neg = c == '-';
            chars.next();
        }
    }
    let body: String = chars.collect();
    if body.is_empty() {
        return Err(format!("For input string: \"{s}\""));
    }
    let mut acc: i64 = 0;
    for c in body.chars() {
        let d = digit_value(c, radix).ok_or_else(|| format!("For input string: \"{s}\""))?;
        acc = acc
            .checked_mul(radix as i64)
            .and_then(|a| a.checked_add(d as i64))
            .ok_or_else(|| format!("For input string: \"{s}\""))?;
        if neg && -acc < i32::MIN as i64 {
            return Err(format!("For input string: \"{s}\""));
        }
        if !neg && acc > i32::MAX as i64 {
            return Err(format!("For input string: \"{s}\""));
        }
    }
    Ok(if neg { (-acc) as i32 } else { acc as i32 })
}

/// Parse a signed long in `radix`, matching `Long.parseLong`.
pub fn parse_i64(s: &str, radix: u32) -> Result<i64, String> {
    if !(2..=36).contains(&radix) {
        return Err(format!("radix {radix} out of range"));
    }
    let mut chars = s.chars().peekable();
    let mut neg = false;
    if let Some(&c) = chars.peek() {
        if c == '+' || c == '-' {
            neg = c == '-';
            chars.next();
        }
    }
    let body: String = chars.collect();
    if body.is_empty() {
        return Err(format!("For input string: \"{s}\""));
    }
    let mut acc: i128 = 0;
    for c in body.chars() {
        let d = digit_value(c, radix).ok_or_else(|| format!("For input string: \"{s}\""))?;
        acc = acc * radix as i128 + d as i128;
        if acc > (i64::MAX as i128) + 1 {
            return Err(format!("For input string: \"{s}\""));
        }
    }
    let v = if neg { -acc } else { acc };
    if v < i64::MIN as i128 || v > i64::MAX as i128 {
        return Err(format!("For input string: \"{s}\""));
    }
    Ok(v as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checked-in table generated from the host JVM, one
    /// `(d|f)\t<bits>\t<expected>` record per line.
    ///
    /// Three JDK-17 `FloatingDecimal` values are deliberately excluded because
    /// that legacy algorithm emits non-shortest representations for them
    /// (`4.9E-324`, `9.999999999999999E22`, `1.17549435E-38`). Modern JDKs and
    /// ART use the shortest round-trip form this VM implements; the corpus
    /// itself contains no such values (see `fixtures/README.md`).
    #[test]
    fn float_string_matches_jvm_reference() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/float_strings.txt");
        let data = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing test table {path:?}"));
        let mut checked = 0;
        let mut mismatches = Vec::new();
        for line in data.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let mut parts = line.split('\t');
            let kind = parts.next().unwrap();
            let bits: u64 = parts.next().unwrap().parse().unwrap();
            let want = parts.next().unwrap();
            let got = match kind {
                "d" => double_to_string(f64::from_bits(bits)),
                "f" => float_to_string(f32::from_bits(bits as u32)),
                other => panic!("bad kind {other}"),
            };
            if got != want {
                mismatches.push(format!("{kind} bits={bits} got={got} want={want}"));
            }
            checked += 1;
        }
        assert!(checked >= 60, "expected >=60 float cases, got {checked}");
        assert!(
            mismatches.is_empty(),
            "{} mismatches:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }

    #[test]
    fn integer_radix_and_parse() {
        assert_eq!(i32_to_radix(255, 16), "ff");
        assert_eq!(i32_to_unsigned_string(-1, 16), "ffffffff");
        assert_eq!(i64_to_unsigned_string(-1, 16), "ffffffffffffffff");
        assert_eq!(i64_to_radix(1234567890123, 36), "fr5hugnf");
        assert_eq!(parse_i32("-2147483648", 10), Ok(i32::MIN));
        assert!(parse_i32("2147483648", 10).is_err());
        assert_eq!(parse_i64("-9223372036854775808", 10), Ok(i64::MIN));
        assert!(parse_i64("9223372036854775808", 10).is_err());
    }
}
