//! Neutralize characters in GitHub-sourced text that could disguise it or inject terminal escapes.
//! They are shown as `<U+202E>` so they stay visible instead of silently reordering or hiding text.
use std::borrow::Cow;

fn visible(c: char) -> String {
    format!("<U+{:04X}>", c as u32)
}

/// Bidi controls, invisible and zero-width format characters (terminals and width tables disagree on
/// how wide they are, which shifts everything after them on the row) and C0/C1 controls (except
/// `\n` and `\t`).
fn risky(c: char) -> bool {
    matches!(c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
        | '\u{200B}' | '\u{200C}' | '\u{2060}' | '\u{FEFF}'
        | '\u{2028}' | '\u{2029}' | '\u{2061}'..='\u{2064}' | '\u{206A}'..='\u{206F}'
        | '\u{00AD}' | '\u{034F}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}'
        | '\u{180B}'..='\u{180F}' | '\u{3164}' | '\u{FFA0}' | '\u{FFF9}'..='\u{FFFB}'
        | '\u{FE00}'..='\u{FE0D}' | '\u{E0100}'..='\u{E01EF}'
        | '\u{1BCA0}'..='\u{1BCA3}' | '\u{1D173}'..='\u{1D17A}' | '\u{E0000}'..='\u{E007F}'
        | '\u{0000}'..='\u{0008}' | '\u{000B}'..='\u{001F}' | '\u{007F}'..='\u{009F}')
}

/// Variation selectors (text/emoji presentation). Terminals disagree with the width tables on
/// "\u{26a0}\u{fe0f}" (1 or 2 cells), which desynchronizes the screen diff and leaves ghost text on
/// the rest of the row, so they are dropped: the base glyph is drawn with its default width.
fn selector(c: char) -> bool {
    matches!(c, '\u{FE0E}' | '\u{FE0F}')
}

/// CRLF and lone CR become `\n` first (GitHub web-written bodies use CRLF), then the rest is neutralized.
pub fn clean(s: &str) -> Cow<'_, str> {
    if s.contains('\r') {
        return Cow::Owned(clean(&s.replace("\r\n", "\n").replace('\r', "\n")).into_owned());
    }
    if !s
        .chars()
        .any(|c| risky(c) || selector(c) || c == '\u{200D}')
    {
        return Cow::Borrowed(s);
    }
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 16);
    for (i, &c) in cs.iter().enumerate() {
        // A joiner is legitimate inside emoji/script sequences (both neighbours non-ASCII), not between plain text.
        let joiner_ok = c == '\u{200D}'
            && i > 0
            && cs.get(i + 1).is_some_and(|n| !n.is_ascii())
            && !cs[i - 1].is_ascii();
        if selector(c) {
            continue;
        }
        if risky(c) || (c == '\u{200D}' && !joiner_ok) {
            out.push_str(&visible(c));
        } else {
            out.push(c);
        }
    }
    if out == s {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(out)
    }
}

/// For a field that is one line (a title, a name, a label): line breaks and tabs become a single space
/// (CRLF is one break), then everything `clean` neutralizes.
pub fn line(s: &str) -> Cow<'_, str> {
    if !s.contains(['\r', '\n', '\t']) {
        return clean(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' | '\n' | '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    Cow::Owned(clean(&out).into_owned())
}

pub fn line_in_place(s: &mut String) {
    if let Cow::Owned(o) = line(s) {
        *s = o;
    }
}

pub fn clean_in_place(s: &mut String) {
    if let Cow::Owned(o) = clean(s) {
        *s = o;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings_are_normalized_but_escapes_stay_visible() {
        assert_eq!(clean("a\r\nb\rc\n"), "a\nb\nc\n");
        assert_eq!(clean("a\r\n\u{1b}[2J"), "a\n<U+001B>[2J");
    }

    #[test]
    fn neutralizes_bidi_zero_width_and_escapes() {
        assert_eq!(clean("a\u{202E}b"), "a<U+202E>b");
        assert_eq!(clean("\u{2066}x\u{2069}"), "<U+2066>x<U+2069>");
        assert_eq!(clean("l\u{200E}r\u{200F}"), "l<U+200E>r<U+200F>");
        assert_eq!(
            clean("zero\u{200B}width\u{FEFF}"),
            "zero<U+200B>width<U+FEFF>"
        );
        assert_eq!(clean("\u{1b}[2J\u{7}"), "<U+001B>[2J<U+0007>");
        assert_eq!(clean("c1\u{9b}"), "c1<U+009B>");
        assert_eq!(
            clean("a\u{200D}b"),
            "a<U+200D>b",
            "joiner between plain letters is risky"
        );
    }

    #[test]
    fn leaves_normal_text_alone() {
        for s in [
            "plain",
            "tab\there",
            "line\nbreak",
            "caf\u{e9} \u{65e5}\u{672c}\u{8a9e}",
            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
        ] {
            assert!(
                matches!(clean(s), Cow::Borrowed(_)),
                "{s:?} should be untouched"
            );
            assert_eq!(clean(s), s);
        }
    }

    #[test]
    fn one_line_fields_turn_breaks_and_tabs_into_a_single_space() {
        assert_eq!(line("crlf\r\nline2"), "crlf line2", "CRLF is one break");
        assert_eq!(line("a\rb\nc\td"), "a b c d");
        assert_eq!(line("x\n\ny"), "x  y", "each break is a space");
        assert_eq!(line("plain"), "plain");
        assert_eq!(
            line("e\u{202e}vil\r\n"),
            "e<U+202E>vil ",
            "and the rest is still neutralized"
        );
    }

    #[test]
    fn invisible_format_characters_are_made_visible_so_widths_cannot_drift() {
        for c in [
            '\u{00AD}',
            '\u{034F}',
            '\u{115F}',
            '\u{1160}',
            '\u{17B4}',
            '\u{180B}',
            '\u{2028}',
            '\u{2029}',
            '\u{2061}',
            '\u{2064}',
            '\u{206A}',
            '\u{3164}',
            '\u{FE00}',
            '\u{FFA0}',
            '\u{FFF9}',
            '\u{E0001}',
            '\u{E0020}',
            '\u{E0100}',
        ] {
            let got = clean(&format!("a{c}b")).into_owned();
            assert_eq!(got, format!("a<U+{:04X}>b", c as u32), "{c:?}");
        }
        // what stays: ordinary text, emoji with a selector (stripped), CJK, combining marks
        assert_eq!(
            clean("caf\u{e9} \u{4e2d}\u{6587} e\u{301} \u{26a0}\u{fe0f}"),
            "caf\u{e9} \u{4e2d}\u{6587} e\u{301} \u{26a0}"
        );
    }
}
