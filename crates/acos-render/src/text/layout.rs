//! Line breaking (Android `android.graphics.text.LineBreaker`).
//!
//! Three strategies are implemented behind one entry point:
//!
//! * `BREAK_STRATEGY_SIMPLE` — the classic greedy break: fill each line until
//!   the next word does not fit, then break at the last opportunity.
//! * `BREAK_STRATEGY_BALANCED` — dynamic-programming line breaking that
//!   minimises the summed squared slack of every line (raggedness), the same
//!   objective Skia's `Lithium`/`BalancedLineBreaker` uses.
//! * `BREAK_STRATEGY_HIGH_QUALITY` — the balanced objective plus a penalty for
//!   a short trailing/widow line, which is what the "high quality" strategy
//!   optimises for on top of balance.
//!
//! Hyphenation is always off, matching
//! `setHyphenationFrequency(HYPHENATION_FREQUENCY_NONE)`. Where a single
//! "word" is wider than the paragraph width, the optimal breakers fall back to
//! character breaks (the greedy behaviour) rather than overflowing silently.

use super::{TextEngine, TextStyle};
use crate::error::RenderResult;

/// Android `LineBreaker.BreakStrategy`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum BreakStrategy {
    Simple = 0,
    HighQuality = 1,
    Balanced = 2,
}

impl BreakStrategy {
    pub fn from_ordinal(o: i32) -> Option<Self> {
        Some(match o {
            0 => BreakStrategy::Simple,
            1 => BreakStrategy::HighQuality,
            2 => BreakStrategy::Balanced,
            _ => return None,
        })
    }

    /// Every strategy is implemented; kept for callers that gate on support.
    pub fn is_supported(self) -> bool {
        true
    }
}

/// A computed line.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct LineBreak {
    /// UTF-16 index (relative to the paragraph start) of the first unit.
    pub start: usize,
    /// UTF-16 index one past the last unit (exclusive).
    pub end: usize,
    /// Measured width in pixels.
    pub width: f32,
    /// Whether the line ended at an explicit newline.
    pub hard_break: bool,
}

/// Android `LineBreaker.ParagraphConstraints`.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct ParagraphConstraints {
    pub width: f32,
    pub first_width: f32,
    pub indent: f32,
}

impl Default for ParagraphConstraints {
    fn default() -> Self {
        ParagraphConstraints {
            width: f32::MAX,
            first_width: f32::MAX,
            indent: 0.0,
        }
    }
}

/// The line breaker.
#[derive(Debug)]
pub struct LineBreaker {
    pub strategy: BreakStrategy,
}

impl Default for LineBreaker {
    fn default() -> Self {
        LineBreaker {
            strategy: BreakStrategy::Simple,
        }
    }
}

impl LineBreaker {
    pub fn new(strategy: BreakStrategy) -> RenderResult<Self> {
        Ok(LineBreaker { strategy })
    }

    /// Break `text` into lines using precomputed per-UTF-16-char advances. This
    /// is what `LineBreaker.computeLineBreaks` uses once `MeasuredText` has
    /// advances cached.
    pub fn compute_breaks_from_advances(
        &self,
        text: &[u16],
        advances: &[f32],
        constraints: &ParagraphConstraints,
    ) -> Vec<LineBreak> {
        match self.strategy {
            BreakStrategy::Simple => greedy(text, advances, constraints),
            BreakStrategy::Balanced => optimal(text, advances, constraints, false),
            BreakStrategy::HighQuality => optimal(text, advances, constraints, true),
        }
    }

    /// Convenience: shape + break.
    pub fn compute_breaks(
        &self,
        engine: &mut TextEngine,
        text: &[u16],
        style: &TextStyle,
        constraints: &ParagraphConstraints,
    ) -> Vec<LineBreak> {
        let shaped = engine.shape(text, style);
        self.compute_breaks_from_advances(text, &shaped.char_advances, constraints)
    }
}

fn is_break_after(text: &[u16], i: usize) -> bool {
    let Some(ch) = char::from_u32(text[i] as u32) else {
        return false;
    };
    if ch == ' ' || ch == '\t' || ch == '-' {
        return true;
    }
    // CJK ideographs and kana can break between characters.
    let script = unicode_script::UnicodeScript::script(&ch);
    matches!(
        script.short_name(),
        "Hani" | "Hira" | "Kana" | "Hang" | "Thai" | "Khmr" | "Laoo" | "Mymr"
    )
}

/// Greedy (`BREAK_STRATEGY_SIMPLE`) line breaking.
fn greedy(text: &[u16], advances: &[f32], constraints: &ParagraphConstraints) -> Vec<LineBreak> {
    let mut lines = Vec::new();
    let mut line_start = 0usize;
    let mut last_break: Option<usize> = None;
    let mut width = 0.0f32;
    let mut limit = constraints.first_width;
    if limit == f32::MAX {
        limit = constraints.width;
    }
    let mut i = 0usize;
    while i < text.len() {
        let unit = text[i];
        if unit == 0x000A {
            // Hard newline: consume it into the line.
            lines.push(LineBreak {
                start: line_start,
                end: i + 1,
                width: width + advances.get(i).copied().unwrap_or(0.0),
                hard_break: true,
            });
            line_start = i + 1;
            width = 0.0;
            last_break = None;
            i += 1;
            limit = constraints.width;
            continue;
        }
        let a = advances.get(i).copied().unwrap_or(0.0);
        if width + a > limit && i > line_start {
            let brk = last_break.filter(|&b| b > line_start).unwrap_or(i);
            // Width of the chosen line `[line_start, brk)`, not of the
            // look-ahead `[line_start, i)` that triggered the overflow.
            let line_width: f32 = advances
                .get(line_start..brk)
                .map(|a| a.iter().sum())
                .unwrap_or(0.0);
            lines.push(LineBreak {
                start: line_start,
                end: brk,
                width: line_width,
                hard_break: false,
            });
            // Skip a single breaking space at the start of the next line.
            line_start = brk;
            if text.get(brk).copied() == Some(0x0020) {
                line_start += 1;
            }
            width = 0.0;
            last_break = None;
            i = line_start;
            limit = constraints.width;
            continue;
        }
        width += a;
        if is_break_after(text, i) {
            last_break = Some(if text[i] == 0x0020 { i } else { i + 1 });
        }
        i += 1;
    }
    if line_start < text.len() || lines.is_empty() {
        lines.push(LineBreak {
            start: line_start,
            end: text.len(),
            width,
            hard_break: false,
        });
    }
    lines
}

/// Dynamic-programming breaker shared by the balanced and high-quality
/// strategies. Chosen so that `Balanced` minimises raggedness; `HighQuality`
/// additionally penalises a very short line before a line break.
fn optimal(
    text: &[u16],
    advances: &[f32],
    constraints: &ParagraphConstraints,
    high_quality: bool,
) -> Vec<LineBreak> {
    // Prefix sums of advances: pre[i] = sum(advances[0..i]).
    let mut pre = vec![0.0f32; text.len() + 1];
    for (i, a) in advances.iter().enumerate() {
        pre[i + 1] = pre[i] + a;
    }
    let width = if constraints.width == f32::MAX {
        f32::MAX
    } else {
        constraints.width.max(0.0)
    };

    let mut lines: Vec<LineBreak> = Vec::new();
    // Paragraphs are delimited by explicit newlines; each is broken
    // independently so a hard break can never be crossed.
    let mut paragraph_start = 0usize;
    let mut i = 0usize;
    while i <= text.len() {
        if i == text.len() || text[i] == 0x000A {
            let paragraph_end = i;
            let has_newline = i < text.len();
            break_paragraph(
                text,
                &pre,
                paragraph_start,
                paragraph_end,
                width,
                constraints.indent,
                high_quality,
                &mut lines,
            );
            if has_newline {
                if let Some(last) = lines.last_mut() {
                    // The newline is part of the hard line, matching greedy.
                    last.end = paragraph_end + 1;
                    last.hard_break = true;
                }
            }
            paragraph_start = i + 1;
        }
        i += 1;
    }
    lines
}

/// Break a single paragraph `[start, end)` and append its lines.
#[allow(clippy::too_many_arguments)]
fn break_paragraph(
    text: &[u16],
    pre: &[f32],
    start: usize,
    end: usize,
    width: f32,
    indent: f32,
    high_quality: bool,
    lines: &mut Vec<LineBreak>,
) {
    if start >= end {
        // Empty paragraph: one empty line (keeps explicit blank lines).
        lines.push(LineBreak {
            start,
            end,
            width: 0.0,
            hard_break: false,
        });
        return;
    }

    let n = end - start;
    // best[j] = minimal cost to break [start, start+j); parent[j] = previous
    // start offset. A sentinel `f32::INFINITY` means unreachable.
    let mut best = vec![f32::INFINITY; n + 1];
    let mut parent = vec![0usize; n + 1];
    best[0] = 0.0;

    for sj in 0..n {
        if !best[sj].is_finite() {
            continue;
        }
        let s = start + sj;
        let indent_here = if s == start { indent } else { 0.0 };
        // Candidate end positions: every position after `s`. Word-boundary
        // breaks are strongly preferred; mid-word breaks are the fallback for
        // over-long words (mirroring greedy's character break).
        for ej in (sj + 1)..=n {
            let ns = start + ej;
            // Trim trailing spaces from the measured line.
            let mut e = ns;
            while e > s && text[e - 1] == 0x0020 {
                e -= 1;
            }
            let w = (pre[e] - pre[s]) + indent_here;
            let word_break = ns == end || is_break_after(text, ns - 1);
            let mut cost = if width == f32::MAX || w <= width {
                let slack = if width == f32::MAX { 0.0 } else { width - w };
                slack * slack
            } else {
                // Overflow: heavily discouraged but still representable.
                1.0e7 + (w - width) * (w - width)
            };
            if !word_break && ns < end {
                // Mid-word break: a real cost, but a valid last resort.
                cost += 1.0e5;
            }
            if high_quality && ns < end && width != f32::MAX && w < width * 0.35 {
                // Widow/short-line penalty: discourage a much shorter line.
                let deficit = width * 0.35 - w;
                cost += deficit * deficit;
            }
            let cand = best[sj] + cost;
            if cand < best[ej] {
                best[ej] = cand;
                parent[ej] = sj;
            }
        }
    }

    // Reconstruct the chosen line starts (ending at `n`).
    let mut starts = vec![n];
    let mut j = n;
    while j > 0 {
        j = parent[j];
        starts.push(j);
    }
    starts.reverse();
    for pair in starts.windows(2) {
        let s = start + pair[0];
        let ns = start + pair[1];
        let mut e = ns;
        while e > s && text[e - 1] == 0x0020 {
            e -= 1;
        }
        let indent_here = if s == start { indent } else { 0.0 };
        lines.push(LineBreak {
            start: s,
            end: e,
            width: (pre[e] - pre[s]) + indent_here,
            hard_break: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn greedy_breaks_at_words() {
        // advances: every char is 10 wide.
        let text = units("aa bb cc dd");
        let advances = vec![10.0f32; text.len()];
        let lb = LineBreaker::default();
        let c = ParagraphConstraints {
            width: 55.0,
            first_width: 55.0,
            indent: 0.0,
        };
        let lines = lb.compute_breaks_from_advances(&text, &advances, &c);
        assert!(lines.len() >= 2, "{lines:?}");
        for l in &lines {
            assert!(l.width <= 55.0 + 10.0, "{l:?}");
        }
    }

    #[test]
    fn hard_newline_forces_break() {
        let text = units("abc\ndef");
        let advances = vec![1.0f32; text.len()];
        let lb = LineBreaker::default();
        let lines =
            lb.compute_breaks_from_advances(&text, &advances, &ParagraphConstraints::default());
        assert_eq!(lines.len(), 2);
        assert!(lines[0].hard_break);
    }

    #[test]
    fn balanced_and_high_quality_are_accepted() {
        assert!(LineBreaker::new(BreakStrategy::Balanced).is_ok());
        assert!(LineBreaker::new(BreakStrategy::HighQuality).is_ok());
    }

    /// Balanced must respect the width limit, cover the text exactly, and be no
    /// worse on raggedness than greedy.
    #[test]
    fn balanced_never_worse_than_greedy_and_covers_text() {
        let text = units("the quick brown fox jumps over the lazy dog");
        // Variable advances so the greedy/balanced difference is visible.
        let advances: Vec<f32> = text
            .iter()
            .map(|&c| if c == 0x0020 { 6.0 } else { 9.0 })
            .collect();
        let c = ParagraphConstraints {
            width: 80.0,
            first_width: 80.0,
            indent: 0.0,
        };
        let greedy = LineBreaker::new(BreakStrategy::Simple)
            .unwrap()
            .compute_breaks_from_advances(&text, &advances, &c);
        let balanced = LineBreaker::new(BreakStrategy::Balanced)
            .unwrap()
            .compute_breaks_from_advances(&text, &advances, &c);

        for lines in [&greedy, &balanced] {
            // Lines advance monotonically and cover the text; the single
            // whitespace at a line break may be dropped.
            let mut cursor = 0usize;
            for l in lines.iter() {
                assert!(l.start >= cursor && l.start <= cursor + 1, "{lines:?}");
                assert!(l.end > l.start, "{lines:?}");
                cursor = l.end;
            }
            assert_eq!(cursor, text.len(), "{lines:?}");
            for l in lines.iter() {
                assert!(l.width <= 80.0 + 1e-3, "{l:?}");
            }
        }
        let ragged = |lines: &[LineBreak]| -> f32 {
            lines.iter().map(|l| (80.0 - l.width).powi(2)).sum()
        };
        assert!(
            ragged(&balanced) <= ragged(&greedy) + 1e-3,
            "balanced {} > greedy {}",
            ragged(&balanced),
            ragged(&greedy)
        );
    }

    #[test]
    fn high_quality_breaks_over_long_word_at_character_boundary() {
        // A single word wider than the line must still produce bounded lines.
        let text = units("supercalifragilisticexpialidocious");
        let advances = vec![10.0f32; text.len()];
        let c = ParagraphConstraints {
            width: 50.0,
            first_width: 50.0,
            indent: 0.0,
        };
        let lines = LineBreaker::new(BreakStrategy::HighQuality)
            .unwrap()
            .compute_breaks_from_advances(&text, &advances, &c);
        assert!(lines.len() > 1);
        for l in &lines {
            assert!(l.width <= 50.0 + 1e-3, "{l:?}");
        }
        assert_eq!(lines.last().unwrap().end, text.len());
    }

    #[test]
    fn blank_lines_are_preserved() {
        let text = units("a\n\nb");
        let advances = vec![5.0f32; text.len()];
        let c = ParagraphConstraints::default();
        let lines = LineBreaker::new(BreakStrategy::Balanced)
            .unwrap()
            .compute_breaks_from_advances(&text, &advances, &c);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].hard_break && lines[1].hard_break);
        assert!(!lines[2].hard_break);
    }
}
