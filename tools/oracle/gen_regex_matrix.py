#!/usr/bin/env python3
"""Generate fixtures/dexprogs/26_regex_matrix/Main.java.

The generated program is a data-driven differential corpus for the ICU-backed
`java.util.regex` natives (`crates/acos-vm/src/natives/icu_regex.rs`).  Each row
is a `(pattern, flags, input, operation, argument)` tuple; the program prints
`NNNN=<result>` lines that are captured from real ART (`run-dexprogs-art.sh`)
and compared byte-for-byte against ACOS.

Only constructs the engine models are emitted here; known divergences live in
`results/vm/REGEX_DIVERGENCES.md` instead of silently failing the fixture.
"""
import json
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
OUT = os.path.join(REPO, "fixtures/dexprogs/26_regex_matrix/Main.java")

# java.util.regex.Pattern flags
CI = 0x02
COMMENTS = 0x04
MULTILINE = 0x08
LITERAL = 0x10
DOTALL = 0x20
UNICODE_CASE = 0x40
CANON_EQ = 0x80
UNICODE_CHARACTER_CLASS = 0x100

cases = []


def add(pat, flags, inp, op="find", arg=""):
    cases.append((pat, flags, inp, op, arg))


def jstr(s):
    return json.dumps(s, ensure_ascii=True)


# --- 1. literals, quantifiers, alternation ---------------------------------
for pat, inp, op, arg in [
    ("abc", "xabcx", "find", ""),
    ("abc", "abc", "matches", ""),
    ("abc", "abcd", "matches", ""),
    ("a*b", "aaab", "matches", ""),
    ("a*b", "b", "matches", ""),
    ("a+b", "ab", "matches", ""),
    ("a+b", "b", "matches", ""),
    ("a?b", "b", "matches", ""),
    ("a?b", "ab", "matches", ""),
    ("a{2}", "aa", "matches", ""),
    ("a{2}", "a", "matches", ""),
    ("a{2,}", "aaaa", "matches", ""),
    ("a{2,4}", "aaa", "matches", ""),
    ("a{2,4}", "aaaaa", "matches", ""),
    ("(ab)+", "ababab", "matches", ""),
    ("(ab)+c", "ababc", "matches", ""),
    ("(a|b)+", "abba", "matches", ""),
    ("a|b|c", "c", "find", ""),
    ("(?:ab)+", "abab", "matches", ""),
    ("a.*b", "axxxb", "matches", ""),
    ("a.*?b", "axxxbxxb", "find", ""),
    ("<.*>", "<a><b>", "replaceAll", "G"),
    ("<.*?>", "<a><b>", "replaceAll", "L"),
    ("\\d+", "a12b345", "findAll", ""),
    ("\\w+", "one two three", "findAll", ""),
    ("\\s+", "a  b\tc", "findAll", ""),
    ("[abc]+", "cabxba", "find", ""),
    ("[^abc]+", "abcxyzabc", "find", ""),
    ("[a-z]+", "ABCdefGHI", "find", ""),
    ("[A-Za-z0-9_]+", "a_1 B-2", "findAll", ""),
]:
    add(pat, 0, inp, op, arg)

# --- 2. anchors and boundaries ---------------------------------------------
for pat, flags, inp, op in [
    ("^abc", 0, "abc", "find"),
    ("^abc", 0, "xabc", "find"),
    ("abc$", 0, "abc", "find"),
    ("abc$", 0, "abcx", "find"),
    ("^abc$", 0, "abc", "matches"),
    ("^b", MULTILINE, "a\nb", "find"),
    ("^b", 0, "a\nb", "find"),
    ("b$", MULTILINE, "a\nb", "find"),
    ("\\Aabc", 0, "abc", "find"),
    ("abc\\z", 0, "abc", "find"),
    ("abc\\Z", 0, "abc\n", "find"),
    ("\\bcat\\b", 0, "cat catalog", "replaceAll"),
    ("\\Bcat", 0, "scat", "find"),
    ("\\G\\w\\d", 0, "a1b2c3", "replaceAll"),
    ("\\R", 0, "a\r\nb\nc", "findAll"),
    ("a\\Rb", 0, "a\r\nb", "find"),
    ("^", MULTILINE, "ab\ncd", "replaceAll"),
    ("$", MULTILINE, "ab\ncd", "replaceAll"),
    ("^a", 0, "a\nb", "find"),
    ("\\b", 0, "abc def", "findAll"),
    ("\\B", 0, "abc", "findAll"),
]:
    add(pat, flags, inp, op, "")

# --- 3. character classes and POSIX ----------------------------------------
for pat, inp, op in [
    ("[a-d[m-p]]", "a-d-m-p", "replaceAll"),
    ("[a-z&&[^bc]]", "azbc", "replaceAll"),
    ("[a-z&&[def]]", "adz", "replaceAll"),
    ("[^a-z]", "a1B2", "findAll",),
    ("[\\d&&[^5]]", "123456", "findAll"),
    ("[\\w&&[^_]]", "a_b c", "findAll"),
    ("[]]", "a]b", "find"),
    ("[^]]", "a]b", "find"),
    ("[\\x41-\\x43]+", "ABCx", "find"),
    ("[\\u0041-\\u0043]+", "ABCx", "find"),
    ("[\\t\\n]+", "a\t\nb", "find"),
    ("[[:alpha:]]+", "abc123", "find"),
    ("[[:digit:]]+", "a12b", "find"),
    ("[[:alnum:]]+", "a-1", "find"),
    ("[[:punct:]]", "a!b", "find"),
    ("[[:graph:]]+", " a b ", "find"),
    ("[[:print:]]+", " a\nb ", "find"),
    ("[[:blank:]]+", "a  b", "find"),
    ("[[:cntrl:]]", "a\u0001b", "find"),
    ("[[:xdigit:]]+", "xyzABC123", "find"),
    ("[[:space:]]+", "a \u00a0b", "find"),
    ("[[:lower:]]+", "aBcD", "find"),
    ("[[:upper:]]+", "aBcD", "find"),
    ("[[:word:]]+", "a_1 b", "find"),
    ("[[:ascii:]]+", "a\u00e9b", "find"),
    ("[\\p{L}]+", "a\u00e9\u5b57", "find"),
    ("[\\p{Nd}]+", "a\u0660\uFF11b", "find"),
]:
    add(pat, 0, inp, op, "")

# --- 4. case-insensitive / full Unicode case folding -----------------------
for pat, flags, inp, op in [
    ("abc", CI, "ABC", "find"),
    ("ABC", CI, "abc", "find"),
    ("\u00c4", CI, "\u00e4", "find"),
    ("\u00e4", CI, "\u00c4", "find"),
    ("\u00df", CI, "ss", "find"),
    ("\u00df", CI, "SS", "find"),
    ("ss", CI, "\u00df", "find"),
    ("SS", CI, "\u1e9e", "find"),
    ("\u1e9e", CI, "ss", "find"),
    ("\u212a", CI, "k", "find"),
    ("k", CI, "\u212a", "find"),
    ("\ufb01", CI, "fi", "find"),
    ("fi", CI, "\ufb01", "find"),
    ("\ufb00", CI, "ff", "find"),
    ("\u03c3", CI, "\u03a3", "find"),
    ("\u03c2", CI, "\u03a3", "find"),
    ("\u03a3", CI, "\u03c2", "find"),
    ("stra\u00dfe", CI, "STRASSE", "find"),
    ("strasse", CI, "STRA\u1e9eE", "find"),
    ("\u0587", CI, "\u0565\u0582", "find"),
    ("[a-z]", CI, "Z", "find"),
    ("[^a]", CI, "A", "find"),
    ("[\u00df]", CI, "\u1e9e", "find"),
    ("[\u00df]", CI, "s", "find"),
    ("(?i)\u00df", 0, "ss", "find"),
    ("(?i:abc)def", 0, "ABCdef", "matches"),
    ("(?i:abc)def", 0, "ABCdEf", "matches"),
    ("(?i)a(?-i:b)c", 0, "AbC", "matches"),
    ("(?i)a(?-i:b)c", 0, "ABC", "matches"),
    ("(a)\\1", CI, "aA", "matches"),
    ("\u00df*", CI, "\u00df\u00df", "matches"),
    ("\u00df", CI, "s\u00df", "find"),
]:
    add(pat, flags, inp, op, "")

# --- 5. flags ---------------------------------------------------------------
for pat, flags, inp, op, arg in [
    ("a.b", 0, "a\nb", "matches", ""),
    ("a.b", DOTALL, "a\nb", "matches", ""),
    ("a.b", DOTALL, "a\rb", "matches", ""),
    ("a.b", 0, "a\rb", "matches", ""),
    ("^b", 0, "a\nb", "find", ""),
    ("^b", MULTILINE, "a\nb", "find", ""),
    ("^b", 1, "a\nb", "find", ""),  # UNIX_LINES
    ("a$", 1, "a\rb", "find", ""),
    ("a.b", 1, "a\rb", "matches", ""),
    ("a b c", COMMENTS, "abc", "matches", ""),
    ("a # comment\nb", COMMENTS, "ab", "matches", ""),
    ("a.b", LITERAL, "a.b", "matches", ""),
    ("a.b", LITERAL, "axb", "matches", ""),
    ("a*b", LITERAL, "a*b", "matches", ""),
    ("a.b", LITERAL | CI, "A.B", "matches", ""),
    ("\u00df", LITERAL | CI, "SS", "matches", ""),
    ("abc", UNICODE_CASE | CI, "ABC", "matches", ""),
    ("abc", CANON_EQ, "abc", "matches", ""),
    ("\\w", UNICODE_CHARACTER_CLASS, "a", "find", ""),
    ("a.b", DOTALL | MULTILINE, "x\na\nb", "find", ""),
    ("(?s)a.b", 0, "a\nb", "matches", ""),
    ("(?m)^b", 0, "a\nb", "find", ""),
    ("(?x)a b c", 0, "abc", "matches", ""),
    ("(?d)^b", 0, "a\nb", "find", ""),
    ("(?i)(?-i)ABC", 0, "ABC", "matches", ""),
    ("(?i)(?-i)ABC", 0, "abc", "matches", ""),
    ("(?U)\\w", 0, "a", "find", ""),
]:
    add(pat, flags, inp, op, arg)

# --- 6. groups, backrefs, named groups -------------------------------------
for pat, flags, inp, op, arg in [
    ("(\\d+)-(\\d+)", 0, "12-34 56-78", "findAll", ""),
    ("(\\w+)\\s+\\1", 0, "hello hello world world", "findAll", ""),
    ("(\\w+)\\s+\\1", 0, "hello world", "find", ""),
    ("(a)(b)(c)", 0, "abc", "find", ""),
    ("(a(b(c)))", 0, "abc", "find", ""),
    ("(a)?b", 0, "b", "find", ""),
    ("(a)?b", 0, "ab", "find", ""),
    ("(a|b)\\1", 0, "aa", "find", ""),
    ("(a|b)\\1", 0, "ab", "find", ""),
    ("(?<year>\\d{4})-(?<month>\\d{2})-(?<day>\\d{2})", 0,
     "Date: 2024-06-15", "find", ""),
    ("(?<y>\\d{4})-(?<m>\\d{2})-(?<d>\\d{2})", 0, "2024-06-15",
     "replaceAll", "${d}/${m}/${y}"),
    ("(?<word>\\w+)\\s+\\k<word>", 0, "hi hi", "find", ""),
    ("(?<x>a)", 0, "a", "named", "x"),
    ("(?<x>a)(?<y>b)", 0, "ab", "named", "y"),
    ("(a)(b)", 0, "ab", "groupCount", ""),
    ("((a)(b))", 0, "ab", "groupCount", ""),
    ("(?:a)(b)", 0, "ab", "groupCount", ""),
    ("(a+)(b+)", 0, "aaabbb", "find", ""),
    ("(\\w+) (\\w+)", 0, "john smith", "replaceAll", "$2 $1"),
    ("(\\w+) (\\w+) (\\w+)", 0, "one two three", "replaceAll", "$3-$2-$1"),
    ("(?<a>x)(?<b>y)", 0, "xy", "replaceAll", "${b}${a}"),
    ("(a)(b)?", 0, "a", "find", ""),
    ("(a)(b)?", 0, "ab", "find", ""),
    ("(a*)*", 0, "aaa", "matches", ""),
    ("(a|ab)(c|bcd)", 0, "abcd", "find", ""),
]:
    add(pat, flags, inp, op, arg)

# --- 7. lookaround ----------------------------------------------------------
for pat, inp, op in [
    ("\\w+(?=\\d)", "foo1 bar2", "replaceAll"),
    ("foo(?!\\d)", "foo1 fooX", "replaceAll"),
    ("(?<=\\$)\\d+", "$100", "replaceAll"),
    ("(?<=100) dollars", "100 dollars", "replaceAll"),
    ("(?<!a)b", "ab cb", "replaceAll"),
    ("a(?=b)", "ab ac", "findAll"),
    ("a(?!b)", "ab ac", "findAll"),
    ("(?<=a)b", "ab cb", "findAll"),
    ("(?<!a)b", "ab cb", "findAll"),
    ("(?<=a{1,3})b", "aaab", "find"),
    ("(?<=a{2})b", "aab ab", "findAll"),
    ("(?<=ab|abc)d", "abcd", "find"),
    ("(?<=^a)b", "ab", "find"),
    ("(?<=(?=a)a)b", "ab", "find"),
    ("(?=.*b).*", "abc", "find"),
    ("(?!.*b).*", "abc", "find"),
    ("(?=(a+))a*b\\1", "aaab", "find"),
    ("(?<=\\d)(?=\\d)", "123", "findAll"),
    ("\\d(?=\\d)", "123", "replaceAll"),
    ("(?<=a)(?<=b)", "ab", "find"),
    ("(?<!x)(?<!y)z", "z", "find"),
    ("a(?=b|c)", "ab ac ad", "findAll"),
    ("(?<=a|b)c", "ac bc dc", "findAll"),
]:
    add(pat, 0, inp, op, "")

# --- 8. possessive / atomic / lazy -----------------------------------------
for pat, inp, op in [
    ("a*+a", "aa", "matches"),
    ("a*+", "aaa", "matches"),
    ("a++", "aaa", "matches"),
    ("a?+", "a", "matches"),
    ("(?>a*)a", "aaaa", "matches"),
    ("(?>a*)a", "aaa", "matches"),
    ("a{2,3}+a", "aaaa", "matches"),
    ("a{2,3}+a", "aaaaa", "matches"),
    ("<.*+>", "<a><b>", "find"),
    ("<.*?>", "<a><b>", "find"),
    ("a+?", "aaa", "find"),
    ("a{2,3}?", "aaaa", "replaceAll"),
    ("(?:a|ab)+c", "ababc", "matches"),
    ("(?>(a|ab)+)c", "ababc", "matches"),
    ("\\d++\\d", "123", "matches"),
]:
    add(pat, 0, inp, op, "")

# --- 9. region / anchoring / transparent bounds ----------------------------
for op, arg, pat, inp in [
    ("regionMatch", "3,6", "\\d+", "abc123def456"),
    ("regionFind", "3,6", "\\d+", "abc123def456"),
    ("regionFindAll", "0,6", "\\d", "abc123def456"),
    ("regionBounds", "3,6", "\\d+", "abc123def456"),
    ("noAnchorMatch", "3,6", "\\d+", "abc123def456"),
    ("noAnchorMatch", "3,6", "\\d+", "xx123def"),
    ("noAnchorMatch", "3,6", "^\\d+$", "xx123def"),
    ("anchorMatch", "3,6", "^\\d+$", "xx123def"),
    ("noAnchorFind", "3,6", "^\\d", "xx123def"),
    ("transparentFind", "3,6", "(?<=x)\\d+", "xx123def"),
    ("opaqueFind", "3,6", "(?<=x)\\d+", "xx123def"),
    ("transparentFind", "3,6", "\\b\\d+", "xx123def"),
    ("opaqueFind", "3,6", "\\b\\d+", "xx123def"),
    ("regionMatch", "0,3", "abc", "abcdef"),
    ("regionMatch", "0,2", "abc", "abcdef"),
    ("regionFind", "0,0", "a*", "abc"),
    ("regionBounds", "0,3", "abc", "abcdef"),
    ("regionBounds", "2,5", "abc", "abcdef"),
    ("noAnchorFind", "2,5", "^", "abcdef"),
    ("anchorFind", "2,5", "^", "abcdef"),
    ("transparentFind", "2,5", "(?<=b)", "abcdef"),
    ("opaqueFind", "2,5", "(?<=b)", "abcdef"),
    ("regionFindAll", "1,4", "\\w", "abcdef"),
]:
    add(pat, 0, inp, op, arg)

# --- 10. hitEnd / requireEnd ------------------------------------------------
for op, pat, inp in [
    ("hitEnd", "a", "xa"),
    ("hitEnd", "a", "xax"),
    ("hitEnd", "\\d", "ab"),
    ("hitEnd", "a*", "aaa"),
    ("hitEnd", "a$", "ba"),
    ("hitEnd", "\\w+", "abc"),
    ("requireEnd", "a$", "ba"),
    ("requireEnd", "a", "ba"),
    ("requireEnd", "a*$", "aaa"),
    ("requireEnd", "\\d+", "12"),
    ("requireEnd", "^a", "ab"),
    ("requireEnd", "a\\z", "ba"),
]:
    add(pat, 0, inp, op, "")

# --- 11. find(int) ----------------------------------------------------------
for pat, inp, start in [
    ("\\d+", "a12b345", 0),
    ("\\d+", "a12b345", 2),
    ("\\d+", "a12b345", 4),
    ("\\w+", "one two three", 4),
    ("\\w+", "one two three", 8),
    ("a", "aaa", 1),
    ("a", "aaa", 2),
    ("a", "aaa", 3),
    ("", "abc", 1),
    ("\\d", "1a2b3", 2),
    ("^b", "ab\nb", 3),
    ("b", "abc", 10),
]:
    add(pat, 0, inp, "findFrom", str(start))

# --- 12. appendReplacement / appendTail ------------------------------------
for pat, inp, repl in [
    ("(\\w+)=(\\w+)", "k1=v1 k2=v2", "$2:$1;"),
    ("\\d+", "a1b22c333", "#"),
    ("(a)(b)", "abab", "$2$1"),
    ("(?<x>\\d+)", "x12y34", "[${x}]"),
    ("\\w+", "one two", "X"),
    ("a", "aaa", "\\$"),
    ("(\\d)", "1 2 3", "$1$1"),
]:
    add(pat, 0, inp, "append", repl)

for arg in ["$1\\", "$1", "\\$", "x$0y", "$12"]:
    add("(a)", 0, "abc", "quote", arg)

# --- 13. split limits -------------------------------------------------------
for pat, inp, limit in [
    (":", "a:b:c", 0),
    (":", "a::b", 0),
    (":", ":a:b", 0),
    (":", "a:b:", 0),
    (":", "a:b:c:d", 2),
    (":", "a:b:c", -1),
    ("\\d+", "a1b22c", 0),
    ("x", "a,b,c", 0),
    (",", "a,b,c,", 0),
    (",", "a,b,c,", -1),
    (",", "a,b,c,", 3),
    ("", "abc", 0),
    ("", "abc", -1),
    ("", "abc", 2),
    ("\\s+", "a  b   c", 0),
    ("\\s+", "a  b   c", 2),
    ("(?<=\\d)(?=\\D)", "a1b2c", 0),
    (",", ",,", 0),
    (",", ",,", -1),
    (",", "a", 0),
]:
    add(pat, 0, inp, "split", str(limit))

# --- 14. String fast paths --------------------------------------------------
for pat, inp, op, arg in [
    ("[a-z]+\\d+", "abc123", "strMatches", ""),
    ("[a-z]+\\d+", "ABC123", "strMatches", ""),
    ("a.b", "a.b", "strMatches", ""),
    ("\\d+", "a1b22c", "strReplaceAll", "#"),
    ("(\\w+) (\\w+)", "john smith", "strReplaceAll", "$2 $1"),
    ("\\d+", "a1b22c", "strReplaceFirst", "#"),
    (":", "a:b:c", "strSplit", "0"),
    (":", "a:b:c", "strSplit", "2"),
    (",", "a,b,", "strSplit", "-1"),
    ("\\s+", "a  b", "strSplit", "0"),
    ("", "abc", "strReplaceAll", "-"),
    ("(?i)abc", "ABC", "strMatches", ""),
    ("\\w+", "hello", "strMatches", ""),
    ("^a.*z$", "abz", "strMatches", ""),
    ("\\d", "abc", "strMatches", ""),
    ("\\d", "abc", "strReplaceAll", "#"),
    ("b", "abc", "strReplaceFirst", "B"),
    ("b", "abcabc", "strSplit", "0"),
    ("x", "abc", "strSplit", "0"),
    ("a", "aaa", "strReplaceAll", "b"),
]:
    add(pat, 0, inp, op, arg)

# --- 15. Unicode properties -------------------------------------------------
for prop, inp in [
    ("L", "\u00e9"), ("L", "5"), ("Lu", "\u00c9"), ("Ll", "\u00e9"),
    ("Lt", "\u01c5"), ("Lm", "\u02b0"), ("Lo", "\u5b57"),
    ("M", "\u0301"), ("Mn", "\u0301"), ("Mc", "\u0903"), ("Me", "\u20dd"),
    ("N", "\u0660"), ("Nd", "\u0660"), ("Nl", "\u2160"), ("No", "\u00bd"),
    ("P", "!"), ("Pc", "_"), ("Pd", "-"), ("Ps", "("), ("Pe", ")"),
    ("Pi", "\u00ab"), ("Pf", "\u00bb"), ("Po", "!"),
    ("S", "$"), ("Sm", "+"), ("Sc", "$"), ("Sk", "^"), ("So", "\u00a9"),
    ("Z", " "), ("Zs", " "), ("Zl", "\u2028"), ("Zp", "\u2029"),
    ("C", "\u0001"), ("Cc", "\u0001"), ("Cf", "\u200b"), ("Co", "\ue000"),
    ("Alphabetic", "\u00e9"), ("IsAlphabetic", "\u00e9"),
    ("White_Space", "\u00a0"), ("Lowercase", "\u00e9"),
    ("Uppercase", "\u00c9"),
    ("javaLowerCase", "\u00e9"), ("javaUpperCase", "\u00c9"),
    ("javaWhitespace", " "), ("javaWhitespace", "\u00a0"),
    ("javaDigit", "\u0660"), ("javaLetter", "\u00e9"),
    ("javaLetterOrDigit", "5"), ("javaLetterOrDigit", "!"),
    ("sc=Greek", "\u03b1"), ("IsGreek", "\u03b1"), ("Greek", "\u03b1"),
    ("sc=Latin", "a"), ("IsLatin", "a"),
    ("sc=Cyrillic", "\u0430"), ("sc=Han", "\u5b57"),
    ("sc=Arabic", "\u0660"),
]:
    add("\\p{" + prop + "}", 0, inp, "find", "")

# Negated properties
for prop, inp in [("L", "5"), ("Nd", "a"), ("White_Space", "a"), ("Lu", "\u00e9")]:
    add("\\P{" + prop + "}", 0, inp, "find", "")

# --- 16. escapes ------------------------------------------------------------
for pat, inp, op in [
    ("\\x41", "A", "matches"),
    ("\\x{1F600}", "\U0001F600", "matches"),
    ("\\u0041", "A", "matches"),
    ("\\101", "A", "matches"),
    ("\\cA", "\u0001", "matches"),
    ("\\Qa.b\\E", "a.b", "matches"),
    ("\\Qa.b\\E", "axb", "matches"),
    ("\\t", "\t", "matches"),
    ("\\n", "\n", "matches"),
    ("\\r", "\r", "matches"),
    ("\\f", "\f", "matches"),
    ("\\a", "\u0007", "matches"),
    ("\\e", "\u001b", "matches"),
    ("\\.", ".", "matches"),
    ("\\*", "*", "matches"),
    ("\\\\", "\\", "matches"),
    ("\\d\\D", "1a", "matches"),
    ("\\w\\W", "a!", "matches"),
    ("\\s\\S", " a", "matches"),
    ("\\h", "\u00a0", "find"),
    ("\\v", "\u000b", "find"),
    ("\\R", "\r\n", "find"),
    ("\\x{1F600}", "\U0001F600", "find"),
    ("\\uD83D\\uDE00", "\U0001F600", "matches"),
    ("[\\x41-\\x43]", "B", "matches"),
    ("[\\u0041-\\u0043]", "C", "matches"),
]:
    add(pat, 0, inp, op, "")

# ===========================================================================
# Emit Main.java
# ===========================================================================
os.makedirs(os.path.dirname(OUT), exist_ok=True)

header = r'''import java.util.regex.Matcher;
import java.util.regex.Pattern;
import java.util.regex.PatternSyntaxException;

/**
 * Differential regex corpus for the ICU-backed java.util.regex natives.
 *
 * GENERATED by tools/oracle/gen_regex_matrix.py -- do not edit by hand.
 * Each row is (pattern, flags, input, operation, argument); the program prints
 * one `NNNN=<result>` line per row.  The expected output is captured from real
 * ART (fixtures/expected-art/dexprogs/26_regex_matrix.stdout).
 */
public class Main {
    static int caseNo = 0;

    static void out(String result) {
        StringBuilder b = new StringBuilder();
        String n = Integer.toString(caseNo + 1);
        for (int i = n.length(); i < 4; i++) b.append('0');
        b.append(n).append('=').append(result);
        System.out.println(b);
        caseNo++;
    }

    static String hex(String s) {
        StringBuilder b = new StringBuilder();
        for (int i = 0; i < s.length(); i++) {
            String h = Integer.toHexString(s.charAt(i));
            for (int k = h.length(); k < 4; k++) b.append('0');
            b.append(h);
        }
        return b.toString();
    }

    static String describe(Matcher m) {
        StringBuilder b = new StringBuilder();
        b.append(m.start()).append(',').append(m.end()).append(':').append(hex(m.group()));
        for (int g = 1; g <= m.groupCount(); g++) {
            b.append(';');
            if (m.start(g) >= 0) {
                b.append(m.start(g)).append(',').append(m.end(g)).append(':')
                 .append(hex(m.group(g)));
            } else {
                b.append('-');
            }
        }
        return b.toString();
    }

    static String joinSplit(String[] parts) {
        StringBuilder b = new StringBuilder();
        for (int i = 0; i < parts.length; i++) {
            if (i > 0) b.append('|');
            b.append(hex(parts[i]));
        }
        return parts.length + ":" + b;
    }

    static void run(String pat, int flags, String input, String op, String arg) {
        try {
            Pattern p = Pattern.compile(pat, flags);
            Matcher m = p.matcher(input);
            switch (op) {
                case "compileOnly": out("ok"); break;
                case "matches": out(Boolean.toString(m.matches())); break;
                case "lookingAt": out(Boolean.toString(m.lookingAt())); break;
                case "find": out(m.find() ? describe(m) : "none"); break;
                case "findAll": {
                    StringBuilder b = new StringBuilder();
                    while (m.find()) {
                        if (b.length() > 0) b.append('|');
                        b.append(describe(m));
                    }
                    out(b.length() == 0 ? "none" : b.toString());
                    break;
                }
                case "replaceAll": out(hex(m.replaceAll(arg))); break;
                case "replaceFirst": out(hex(m.replaceFirst(arg))); break;
                case "split": out(joinSplit(p.split(input, Integer.parseInt(arg)))); break;
                case "findFrom": out(m.find(Integer.parseInt(arg)) ? describe(m) : "none"); break;
                case "named": out(m.find() && m.start(arg) >= 0 ? hex(m.group(arg)) : "none"); break;
                case "groupCount": out(Integer.toString(m.groupCount())); break;
                case "quote": out(hex(Matcher.quoteReplacement(arg))); break;
                case "append": {
                    StringBuilder b = new StringBuilder();
                    while (m.find()) m.appendReplacement(b, arg);
                    m.appendTail(b);
                    out(hex(b.toString()));
                    break;
                }
                case "hitEnd": {
                    boolean f = m.find();
                    out(f + ":" + m.hitEnd());
                    break;
                }
                case "requireEnd": {
                    boolean f = m.matches();
                    out(f + ":" + m.requireEnd());
                    break;
                }
                case "regionBounds": {
                    String[] r = arg.split(",");
                    m.region(Integer.parseInt(r[0]), Integer.parseInt(r[1]));
                    out(m.regionStart() + "," + m.regionEnd());
                    break;
                }
                case "regionMatch": {
                    String[] r = arg.split(",");
                    m.region(Integer.parseInt(r[0]), Integer.parseInt(r[1]));
                    boolean f = m.matches();
                    out(f + ":" + (f ? describe(m) : "none"));
                    break;
                }
                case "regionFind": {
                    String[] r = arg.split(",");
                    m.region(Integer.parseInt(r[0]), Integer.parseInt(r[1]));
                    out(m.find() ? describe(m) : "none");
                    break;
                }
                case "regionFindAll": {
                    String[] r = arg.split(",");
                    m.region(Integer.parseInt(r[0]), Integer.parseInt(r[1]));
                    StringBuilder b = new StringBuilder();
                    while (m.find()) {
                        if (b.length() > 0) b.append('|');
                        b.append(describe(m));
                    }
                    out(b.length() == 0 ? "none" : b.toString());
                    break;
                }
                case "noAnchorMatch": {
                    String[] r = arg.split(",");
                    m.region(Integer.parseInt(r[0]), Integer.parseInt(r[1]));
                    m.useAnchoringBounds(false);
                    boolean f = m.matches();
                    out(f + ":" + (f ? describe(m) : "none"));
                    break;
                }
                case "anchorMatch": {
                    String[] r = arg.split(",");
                    m.region(Integer.parseInt(r[0]), Integer.parseInt(r[1]));
                    m.useAnchoringBounds(true);
                    boolean f = m.matches();
                    out(f + ":" + (f ? describe(m) : "none"));
                    break;
                }
                case "noAnchorFind": {
                    String[] r = arg.split(",");
                    m.region(Integer.parseInt(r[0]), Integer.parseInt(r[1]));
                    m.useAnchoringBounds(false);
                    out(m.find() ? describe(m) : "none");
                    break;
                }
                case "anchorFind": {
                    String[] r = arg.split(",");
                    m.region(Integer.parseInt(r[0]), Integer.parseInt(r[1]));
                    m.useAnchoringBounds(true);
                    out(m.find() ? describe(m) : "none");
                    break;
                }
                case "transparentFind": {
                    String[] r = arg.split(",");
                    m.region(Integer.parseInt(r[0]), Integer.parseInt(r[1]));
                    m.useTransparentBounds(true);
                    out(m.find() ? describe(m) : "none");
                    break;
                }
                case "opaqueFind": {
                    String[] r = arg.split(",");
                    m.region(Integer.parseInt(r[0]), Integer.parseInt(r[1]));
                    m.useTransparentBounds(false);
                    out(m.find() ? describe(m) : "none");
                    break;
                }
                case "strMatches": out(Boolean.toString(input.matches(pat))); break;
                case "strReplaceAll": out(hex(input.replaceAll(pat, arg))); break;
                case "strReplaceFirst": out(hex(input.replaceFirst(pat, arg))); break;
                case "strSplit": out(joinSplit(input.split(pat, Integer.parseInt(arg)))); break;
                default: out("BADOP:" + op);
            }
        } catch (PatternSyntaxException e) {
            out("PatternSyntaxException");
        } catch (IllegalArgumentException e) {
            out("IllegalArgumentException");
        } catch (Throwable t) {
            out(t.getClass().getName());
        }
    }

    public static void main(String[] args) {
'''

body = ["        run(%s, %d, %s, %s, %s);" % (
    jstr(p), f, jstr(i), jstr(o), jstr(a)) for (p, f, i, o, a) in cases]

footer = r'''    }
}
'''

with open(OUT, "w", encoding="utf-8") as fh:
    fh.write(header)
    fh.write("\n".join(body))
    fh.write("\n")
    fh.write(footer)

print("wrote %s with %d cases" % (OUT, len(cases)))
