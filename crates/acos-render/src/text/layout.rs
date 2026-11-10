//! Line breaking (Android `android.graphics.text.LineBreaker`).
//!
//! Only the greedy (`BREAK_STRATEGY_SIMPLE`) strategy is implemented; the
//! high-quality/balanced strategies are reported as
//! `RENDER_UNSUPPORTED_OP` rather than silently downgraded. Hyphenation is
//! always off, matching `setHyphenationFrequency(HYPHENATION_FREQUENCY_NONE)`.

use super::{TextEngine, TextStyle};
use crate::error::{RenderError, RenderResult};

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

    pub fn is_supported(self) -> bool {
        self == BreakStrategy::Simple
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
        if !strategy.is_supported() {
            return Err(RenderError::unsupported(format!(
                "LineBreaker strategy {strategy:?}"
            )));
        }
        Ok(LineBreaker { strategy })
    }

    /// Greedy line breaking over precomputed per-UTF-16-char advances. This is
    /// what `LineBreaker.computeLineBreaks` uses once `MeasuredText` has
    /// advances cached.
    pub fn compute_breaks_from_advances(
        &self,
        text: &[u16],
        advances: &[f32],
        constraints: &ParagraphConstraints,
    ) -> Vec<LineBreak> {
        let mut lines = Vec::new();
        let mut line_start = 0usize;
        let mut last_break: Option<usize> = None;
        let mut width = 0.0f32;
        let mut limit = if lines.is_empty() {
            constraints.first_width
        } else {
            constraints.width
        };
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
                lines.push(LineBreak {
                    start: line_start,
                    end: brk,
                    width,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greedy_breaks_at_words() {
        // advances: every char is 10 wide.
        let text: Vec<u16> = "aa bb cc dd".encode_utf16().collect();
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
        let text: Vec<u16> = "abc\ndef".encode_utf16().collect();
        let advances = vec![1.0f32; text.len()];
        let lb = LineBreaker::default();
        let lines = lb.compute_breaks_from_advances(&text, &advances, &ParagraphConstraints::default());
        assert_eq!(lines.len(), 2);
        assert!(lines[0].hard_break);
    }

    #[test]
    fn high_quality_is_unsupported() {
        let err = LineBreaker::new(BreakStrategy::Balanced).unwrap_err();
        assert_eq!(err.code(), "RENDER_UNSUPPORTED_OP");
    }
}
