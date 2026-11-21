//! How much room text takes on screen, in columns. A letter of Chinese,
//! Japanese or Korean takes two; a mark that joins the letter before it
//! takes none. Anything that decides whether words fit a line counts with
//! these, never in characters.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The columns `text` takes.
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// The columns one character takes. A control character takes none.
pub fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// The longest start of `text` that is no wider than `cols`.
pub fn take(text: &str, cols: usize) -> &str {
    let mut used = 0;
    for (at, c) in text.char_indices() {
        used += char_width(c);
        if used > cols {
            return &text[..at];
        }
    }
    text
}

/// `text` without its first `cols` columns. A double-width letter that
/// straddles the cut goes too.
pub fn skip(text: &str, cols: usize) -> &str {
    let mut used = 0;
    for (at, c) in text.char_indices() {
        if used >= cols {
            return &text[at..];
        }
        used += char_width(c);
    }
    ""
}

/// `text` in at most `cols` columns, cut at its end and the cut marked.
pub fn clip(text: &str, cols: usize) -> String {
    if width(text) <= cols {
        return text.to_string();
    }
    if cols == 0 {
        return String::new();
    }
    format!("{}…", take(text, cols - 1))
}

/// `text` with spaces after it, up to `cols` columns.
pub fn pad(text: &str, cols: usize) -> String {
    format!("{text}{}", " ".repeat(cols.saturating_sub(width(text))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_double_width_letter_counts_as_two_columns() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("日本語"), 6);
        assert_eq!(width("a日b"), 4);
        // A mark that joins the letter before it takes no room.
        assert_eq!(width("e\u{301}"), 1);
    }

    #[test]
    fn text_is_cut_by_columns_and_never_through_a_letter() {
        assert_eq!(take("日本語", 4), "日本");
        assert_eq!(take("日本語", 5), "日本", "half a letter is not shown");
        assert_eq!(take("abc", 9), "abc");
        assert_eq!(clip("日本語", 6), "日本語");
        assert_eq!(clip("日本語", 5), "日本…");
        assert_eq!(clip("日本語", 4), "日…");
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("abc", 0), "");
        for cols in 0..12 {
            assert!(width(&clip("a日本語bc漢字", cols)) <= cols, "{cols}");
        }
        assert_eq!(skip("a日本b", 3), "本b");
        assert_eq!(skip("a日本b", 2), "本b", "the letter the cut falls in goes too");
        assert_eq!(skip("ab", 5), "");
        assert_eq!(pad("日本", 6), "日本  ");
        assert_eq!(pad("abcdef", 3), "abcdef");
    }
}
