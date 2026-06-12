fn is_cjk_breakable(c: char) -> bool {
    matches!(c,
        '\u{2E80}'..='\u{9FFF}' |
        '\u{F900}'..='\u{FAFF}' |
        '\u{FE30}'..='\u{FE4F}' |
        '\u{FF00}'..='\u{FFEF}' |
        '\u{20000}'..='\u{2FA1F}'
    )
}

pub fn wrap_text_with_continuation(text: &str, max_width: usize) -> (Vec<String>, Vec<bool>) {
    use unicode_width::UnicodeWidthChar;

    if max_width == 0 {
        return (vec![text.to_string()], vec![false]);
    }

    let mut lines = Vec::new();
    let mut flags = Vec::new();

    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            flags.push(false);
            continue;
        }

        let chars: Vec<char> = paragraph.chars().collect();
        let mut pos = 0;
        let mut is_first_line = true;

        while pos < chars.len() && chars[pos].is_whitespace() {
            pos += 1;
        }
        if pos >= chars.len() {
            lines.push(String::new());
            flags.push(false);
            continue;
        }

        while pos < chars.len() {
            let mut line_width = 0usize;
            let mut line_end = pos;
            let mut last_break: Option<usize> = None;

            while line_end < chars.len() {
                let ch = chars[line_end];
                let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);

                if line_width + ch_width > max_width {
                    break;
                }

                line_width += ch_width;
                line_end += 1;

                if ch.is_whitespace() || is_cjk_breakable(ch) {
                    last_break = Some(line_end);
                }

                if line_end < chars.len()
                    && is_cjk_breakable(chars[line_end])
                    && !ch.is_whitespace()
                {
                    last_break = Some(line_end);
                }
            }

            if line_end >= chars.len() {
                let line_str: String = chars[pos..].iter().collect();
                lines.push(line_str.trim_end().to_string());
                flags.push(!is_first_line);
                break;
            }

            let break_at = if let Some(bp) = last_break {
                if bp > pos { bp } else { line_end.max(pos + 1) }
            } else {
                line_end.max(pos + 1)
            };

            let line_str: String = chars[pos..break_at].iter().collect();
            lines.push(line_str.trim_end().to_string());
            flags.push(!is_first_line);
            is_first_line = false;

            pos = break_at;
            while pos < chars.len() && chars[pos].is_whitespace() {
                pos += 1;
            }
        }
    }

    if lines.is_empty() {
        lines.push(String::new());
        flags.push(false);
    }

    (lines, flags)
}

/// Horizontal bar: `filled` `█` cells padded to `width` with `░`. Saturating
/// by construction — a `filled` exceeding `width` (ratio > 1 from an
/// out-of-range value) renders full instead of panicking on `usize` underflow.
/// The single home for bar strings; lint #44 forbids bare `repeat(w - filled)`.
pub fn hbar(filled: usize, width: usize) -> String {
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// Truncate `s` to `max_width` display columns (UnicodeWidth-aware), ending
/// with `…` when cut. The single home for display-name truncation — render
/// sites must call this, never `chars().take(n)` (which drops the ellipsis
/// and miscounts CJK width). Lint #43 enforces no re-implementation.
pub fn truncate_with_ellipsis(s: &str, max_width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if UnicodeWidthStr::width(s) <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut width = 0;
    let mut result = String::new();
    for ch in s.chars() {
        let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_w > max_width.saturating_sub(1) {
            break;
        }
        result.push(ch);
        width += ch_w;
    }
    result.push('…');
    result
}

/// Render a `part / total` ratio as an integer percentage. When the raw
/// share is non-zero but rounds down to zero, returns the `less-than`
/// indicator so tiny non-zero rows in ranked panels (Projects, Languages)
/// stay distinguishable from true zeroes.
pub fn format_pct(part: u64, total: u64) -> String {
    if part == 0 || total == 0 {
        return "0%".to_string();
    }
    let pct = (part as f64 / total as f64 * 100.0) as u32;
    if pct == 0 {
        "<1%".to_string()
    } else {
        format!("{pct}%")
    }
}

/// Floating-point variant of `format_pct` for cost / fraction shares where
/// the inputs are naturally `f64` (e.g. dollars). Same sub-one-percent
/// semantics as the integer variant — see [`format_pct`].
pub fn format_pct_f64(part: f64, total: f64) -> String {
    if part <= 0.0 || total <= 0.0 {
        return "0%".to_string();
    }
    let pct = (part / total * 100.0) as u32;
    if pct == 0 {
        "<1%".to_string()
    } else {
        format!("{pct}%")
    }
}

pub fn format_number(n: u64) -> String {
    let (divisor, suffix) = match n {
        n if n >= 1_000_000_000_000 => (1_000_000_000_000.0, "T"),
        n if n >= 1_000_000_000 => (1_000_000_000.0, "B"),
        n if n >= 1_000_000 => (1_000_000.0, "M"),
        n if n >= 1_000 => (1_000.0, "K"),
        _ => return n.to_string(),
    };

    let v = n as f64 / divisor;
    if v >= 100.0 {
        let rounded = v.round() as u64;
        format!("{rounded}{suffix}")
    } else if v >= 10.0 {
        format!("{v:.1}{suffix}")
    } else {
        format!("{v:.2}{suffix}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextSegment {
    Plain(String),
    Code {
        lang: Option<String>,
        content: String,
    },
}

pub fn parse_text_with_code_blocks(text: &str) -> Vec<TextSegment> {
    let mut segments = Vec::new();
    let mut current_plain = String::new();
    let mut in_code_block = false;
    let mut code_lang: Option<String> = None;
    let mut code_content = String::new();

    for line in text.lines() {
        if line.starts_with("```") {
            if in_code_block {
                segments.push(TextSegment::Code {
                    lang: code_lang.take(),
                    content: std::mem::take(&mut code_content),
                });
                in_code_block = false;
            } else {
                if !current_plain.is_empty() {
                    segments.push(TextSegment::Plain(std::mem::take(&mut current_plain)));
                }
                in_code_block = true;
                let lang_str = line.trim_start_matches('`').trim();
                code_lang = if lang_str.is_empty() {
                    None
                } else {
                    Some(lang_str.to_string())
                };
            }
        } else if in_code_block {
            if !code_content.is_empty() {
                code_content.push('\n');
            }
            code_content.push_str(line);
        } else {
            if !current_plain.is_empty() {
                current_plain.push('\n');
            }
            current_plain.push_str(line);
        }
    }

    if in_code_block && !code_content.is_empty() {
        segments.push(TextSegment::Code {
            lang: code_lang,
            content: code_content,
        });
    } else if !current_plain.is_empty() {
        segments.push(TextSegment::Plain(current_plain));
    }

    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn hbar_fills_and_pads_to_width() {
        assert_eq!(UnicodeWidthStr::width(hbar(3, 10).as_str()), 10);
        assert_eq!(hbar(0, 4), "░░░░");
        assert_eq!(hbar(4, 4), "████");
    }

    #[test]
    fn hbar_saturates_when_filled_exceeds_width() {
        // ratio > 1 (out-of-range value) must render full, never panic on
        // usize underflow.
        assert_eq!(hbar(99, 5), "█████");
        // width 0 is the other underflow edge — must be empty, not panic.
        assert_eq!(hbar(5, 0), "");
    }

    #[test]
    fn truncate_with_ellipsis_is_display_width_aware() {
        // CJK chars are 2 columns wide; the cut must count COLUMNS, not chars
        // — a chars().take() regression overflows the cell on CJK names.
        let out = truncate_with_ellipsis("日本語の長い名前", 8);
        assert!(out.ends_with('…'), "cut output must end with ellipsis");
        assert!(UnicodeWidthStr::width(out.as_str()) <= 8);

        // A 2-wide char that doesn't fit in the final column must under-fill
        // rather than overflow.
        let tight = truncate_with_ellipsis("ああああ", 6);
        assert!(UnicodeWidthStr::width(tight.as_str()) <= 6);

        // Within budget → unchanged; zero budget → empty.
        assert_eq!(truncate_with_ellipsis("abc", 8), "abc");
        assert_eq!(truncate_with_ellipsis("abc", 0), "");
    }

    #[test]
    fn test_format_number_small() {
        assert_eq!(format_number(999), "999");
    }

    #[test]
    fn format_pct_zero_part_renders_zero() {
        assert_eq!(format_pct(0, 100), "0%");
    }

    #[test]
    fn format_pct_zero_total_renders_zero() {
        assert_eq!(format_pct(50, 0), "0%");
    }

    #[test]
    fn format_pct_sub_one_percent_uses_less_than_glyph() {
        // 5 / 1000 = 0.5% → rounds to 0%; should surface as <1% so it's
        // distinguishable from a true 0% row.
        assert_eq!(format_pct(5, 1000), "<1%");
    }

    #[test]
    fn format_pct_normal_integer_share() {
        assert_eq!(format_pct(250, 1000), "25%");
    }

    #[test]
    fn test_format_number_thousands() {
        assert_eq!(format_number(1500), "1.50K");
        assert_eq!(format_number(15000), "15.0K");
        assert_eq!(format_number(150000), "150K");
    }

    #[test]
    fn test_format_number_millions() {
        assert_eq!(format_number(1_500_000), "1.50M");
        assert_eq!(format_number(15_000_000), "15.0M");
    }

    #[test]
    fn test_format_number_billions() {
        assert_eq!(format_number(1_500_000_000), "1.50B");
    }

    #[test]
    fn test_format_number_trillions() {
        assert_eq!(format_number(1_500_000_000_000), "1.50T");
    }

    #[test]
    fn test_wrap_text_with_continuation_flags() {
        let (lines, flags) = wrap_text_with_continuation("hello world foo bar", 10);
        assert_eq!(lines, vec!["hello", "world foo", "bar"]);
        assert_eq!(flags, vec![false, true, true]);
    }

    #[test]
    fn test_wrap_text_with_continuation_paragraphs() {
        let (lines, flags) = wrap_text_with_continuation("first paragraph\n\nsecond paragraph", 80);
        assert_eq!(lines, vec!["first paragraph", "", "second paragraph"]);
        assert_eq!(flags, vec![false, false, false]);
    }

    #[test]
    fn test_wrap_text_with_continuation_cjk() {
        let (lines, flags) = wrap_text_with_continuation("あいうえおか", 8);
        assert_eq!(lines, vec!["あいうえ", "おか"]);
        assert_eq!(flags, vec![false, true]);
    }

    #[test]
    fn test_format_number_zero() {
        assert_eq!(format_number(0), "0");
    }

    #[test]
    fn test_format_number_max() {
        let result = format_number(u64::MAX);
        assert!(result.ends_with('T'));
    }

    #[test]
    fn test_parse_text_with_code_blocks_simple() {
        let text = "Hello\n```rust\nfn main() {}\n```\nWorld";
        let segments = parse_text_with_code_blocks(text);
        assert_eq!(segments.len(), 3);
        assert!(matches!(&segments[0], TextSegment::Plain(s) if s == "Hello"));
        assert!(
            matches!(&segments[1], TextSegment::Code { lang: Some(l), content: c } if l == "rust" && c == "fn main() {}")
        );
        assert!(matches!(&segments[2], TextSegment::Plain(s) if s == "World"));
    }

    #[test]
    fn test_parse_text_with_code_blocks_multiple() {
        let text = "A\n```\ncode1\n```\nB\n```python\ncode2\n```\nC";
        let segments = parse_text_with_code_blocks(text);
        assert_eq!(segments.len(), 5);
    }

    #[test]
    fn test_parse_text_with_code_blocks_no_lang() {
        let text = "```\nno language\n```";
        let segments = parse_text_with_code_blocks(text);
        assert_eq!(segments.len(), 1);
        assert!(matches!(&segments[0], TextSegment::Code { lang: None, .. }));
    }

    #[test]
    fn test_parse_plain_text_only() {
        let text = "Just plain text\nno code blocks";
        let segments = parse_text_with_code_blocks(text);
        assert_eq!(segments.len(), 1);
        assert!(matches!(&segments[0], TextSegment::Plain(_)));
    }

    #[test]
    fn test_parse_unclosed_code_block() {
        let text = "Start\n```rust\nunclosed code";
        let segments = parse_text_with_code_blocks(text);
        assert_eq!(segments.len(), 2);
        assert!(matches!(&segments[1], TextSegment::Code { .. }));
    }

    #[test]
    fn test_parse_empty_string() {
        let segments = parse_text_with_code_blocks("");
        assert!(segments.is_empty());
    }

    #[test]
    fn test_parse_only_backticks() {
        let segments = parse_text_with_code_blocks("```");
        assert!(segments.is_empty());
    }

    #[test]
    fn test_parse_empty_code_block() {
        let segments = parse_text_with_code_blocks("```\n```");
        assert_eq!(segments.len(), 1);
        assert!(matches!(
            &segments[0],
            TextSegment::Code {
                lang: None,
                content
            } if content.is_empty()
        ));
    }
}
