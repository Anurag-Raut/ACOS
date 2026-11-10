# ICU regex divergence log (`java.util.regex` on Android/ART)

Scope: the ICU-backed regex natives in
`crates/acos-vm/src/natives/icu_regex.rs`, i.e. what runs behind
`com.android.icu.util.regex.PatternNative` / `MatcherNative` when the real
Android 14 boot classpath is used (`--corelib real`).

## Reference tier

* **ART / API 34 (authoritative).** A 386-case differential corpus,
  `fixtures/dexprogs/26_regex_matrix`, is compiled to DEX and executed on the
  `acos-oracle-34` emulator with `dalvikvm64`. The captured output is checked
  in as `fixtures/expected-art/dexprogs/26_regex_matrix.stdout` and is
  deterministic across three ART runs. ACOS reproduces it byte-for-byte in both
  normal mode and `ACOS_GC_STRESS=1`.
* **Host JVM (advisory).** `fixtures/expected/dexprogs/26_regex_matrix.stdout`
  is the OpenJDK 17 output. It differs from ART in 43 of 386 cases (POSIX
  classes, `\d`/`\w`, case folding, …) and is *not* used to grade the regex
  corpus; it is kept only for the generic fixture layout.

Operations covered by the corpus: `matches`, `lookingAt`, `find`, repeated
`find`, `find(int)`, `replaceAll`/`replaceFirst`, `appendReplacement` +
`appendTail`, `split` limits, `groupCount`, named groups, `quoteReplacement`,
`region`/`regionStart`/`regionEnd`, anchoring bounds, transparent bounds,
`hitEnd`/`requireEnd`, the `Pattern.flags` constants, and the
`String.matches`/`replaceAll`/`replaceFirst`/`split` fast paths.

## Divergences fixed in this change

| Area | Before | After (verified against ART) |
|---|---|---|
| Full Unicode case folding | simple first-scalar folding only; `ß`↔`ss`, `ﬁ`↔`fi`, Kelvin sign, final sigma mishandled | full case-fold table (`icu_regex_casefold.rs`) + `Inst::FoldStr` for literal runs: `(?i)ß` matches `ss`/`SS`, `(?i)ss` matches `ß`, `(?i)straße` matches `STRASSE`, … |
| Case-insensitive classes | `CASE_INSENSITIVE` was ignored inside `[...]` | folded membership inside ranges/predicates, applied *inside* negation so `(?i)[^a]` rejects `A` |
| `\d` | any `is_numeric()` (No/Nl included) | `\p{Nd}` only (`\d` does not match Roman numerals or `½`) |
| `\w`, `\b` | alnum/underscore | `Alphabetic \| Mark \| Nd \| Pc \| Join_Control` |
| `\p{...}` | handful of std predicates | all general categories + `L/M/N/P/S/Z/C` groups (`unicode-properties`), scripts via `sc=`/`Script=`/`Is` (`unicode-script`), POSIX `[[:name:]]`, `java*` properties |
| `\p{javaWhitespace}` | Unicode `White_Space` | `Character.isWhitespace` (excludes NBSP, U+0085; includes U+001C–U+001F) |
| `\p{XDigit}` | ASCII hex + numeric | Unicode `Hex_Digit` (fullwidth forms included) |
| `LITERAL` | ignored | pattern matched literally (still case-folded with `CASE_INSENSITIVE`) |
| Capturing group numbering | innermost group got the lowest number | outer-first numbering, matching `(a(b(c)))` |
| Look-behind | returned the first sub-match, so alternation aliases failed | `must_end` forces a sub-match ending at the look-behind position; `(?<=ab|abc)d` works |
| Unbounded look-behind | accepted | rejected with `PatternSyntaxException` (ART rejects `(?<=a+)b`) |
| Empty-body repetition | `(a*)*` looped forever | hidden loop slot (`SaveLoop`/`LoopEnd`) breaks empty iterations |
| Catastrophic backtracking | ran until the wall clock | `ACOS_REGEX_MAX_STEPS` step budget (default 20M) → structured `UNSUPPORTED_JAVA_API` |
| `hitEnd` | set on every failed atom | set only when consuming past the input end (and by end anchors at end), matching ART |
| Opaque look-behind bounds | clamped at region start | ART lets look-behind see before the region start; only look-ahead is clamped at the region end |
| `(?U)` | accepted as no-op | `PatternSyntaxException` (ART behaviour) |
| `\x{110000}` | accepted silently | `PatternSyntaxException` (codepoint range check) |
| `\b{g}` | parsed as `\b` + literal `{g}` | `PatternSyntaxException` (ART behaviour) |

Regression tests for the fixed rows live in
`crates/acos-vm/src/natives/icu_regex.rs` (`mod tests`) and are run with
`cargo test -p acos-vm --lib icu_regex`.

## Remaining divergences (documented, not silent)

These constructs are deliberately **not** in the graded corpus because ACOS and
ART still differ. They are recorded here with representative test names.

1. **Extended grapheme clusters `\X`.**
   Test: `regex_divergence_grapheme_cluster_x`.
   ART (`\X` on `e\u0301`) matches the two-code-unit cluster; ACOS returns
   `UNSUPPORTED_JAVA_API` (no `UNSUPPORTED_REGEX: \X …` support). Implementing
   this needs a UAX #29 grapheme segmenter (e.g. `unicode-segmentation`), which
   is not in the vendored dependency set.

2. **Unicode blocks `\p{blk=…}`, `\p{Block=…}`, `\p{In…}`.**
   Test: `regex_divergence_unicode_blocks`.
   ART resolves `\p{blk=Greek}` and `\p{InBasic_Latin}`; ACOS returns
   `UNSUPPORTED_JAVA_API`. Script-aligned `In<Script>` names happen to resolve
   via the script tables, but block-only names (e.g. `Basic_Latin`) do not.
   A block table would be required.

3. **Binary properties other than the modelled four.**
   Test: `regex_divergence_binary_properties`.
   `Alphabetic`, `White_Space`, `Lowercase`, `Uppercase` (and their `Is`/`java`
   aliases) are modelled; properties such as `\p{Emoji}`,
   `\p{Noncharacter_Code_Point}` or `\p{Ideographic}` are
   `UNSUPPORTED_JAVA_API`.

4. **Full case folding across quantifier boundaries.**
   Test: `regex_divergence_fold_across_quantifier`.
   Folding is applied to contiguous case-insensitive literal runs. A fold that
   would need to consume input matched by a quantification of a *single* folded
   character (e.g. the exact extent of `(?i)ß*` against `ss`) is not modelled;
   the boolean result usually agrees with ART but the matched extent can differ.

5. **`\p{Cs}` / `\p{Cn}` edge membership.**
   Test: `regex_divergence_unassigned_surrogates`.
   `Cs` (surrogate) can never be produced by the UTF-16 → code-point input
   conversion, and `Cn` depends on the exact Unicode version bundled with the
   platform ICU (16.0 tables are embedded); a few code points may differ on
   older/newer ICU.

6. **`hitEnd`/`requireEnd` inside lookaround.**
   Test: `regex_divergence_hit_end_lookaround`.
   End-anchor and consuming-instruction behaviour matches ART
   (`hitEnd` on `a$`, `a*`; `requireEnd` on `a$`, `\d+`), but the flags are not
   exhaustively pinned for every nested lookaround shape.

## Reproducing

```
# authoritative ART capture (emulator must be running)
tools/oracle/emulator.sh start
ADB=/home/Qui/Android/Sdk/platform-tools/adb
$ADB -s emulator-5554 push fixtures/build/dexprogs/26_regex_matrix/classes.dex /data/local/tmp/m26.dex
$ADB -s emulator-5554 shell 'cd /data/local/tmp && dalvikvm64 -cp m26.dex Main > m26.out'

# ACOS side
tools/guarded.sh --mem 4G --time 180 -- target/release/acos run-dex \
    fixtures/build/dexprogs/26_regex_matrix/classes.dex --main Main --corelib real --json
```
