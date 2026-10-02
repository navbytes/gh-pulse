use crate::diff::DLine;
#[cfg(feature = "syntax")]
use crate::diff::Op;
use ratatui::style::Color;
#[cfg(feature = "syntax")]
use std::collections::HashMap;
#[cfg(feature = "syntax")]
use syntect::{
    easy::HighlightLines,
    highlighting::{Theme as SynTheme, ThemeSet},
    parsing::{SyntaxReference, SyntaxSet},
};

pub type Spans = Vec<(Color, String)>;

#[cfg(feature = "syntax")]
struct FileHl {
    syntax: &'static SyntaxReference,
    plain: bool,
    done: usize,
    spans: Vec<Spans>,
    old: HighlightLines<'static>,
    new: HighlightLines<'static>,
}

/// Incremental, cached per-file syntax highlighter: only lines up to the visible window are processed.
#[cfg(feature = "syntax")]
pub struct Hl {
    ps: &'static SyntaxSet,
    theme: &'static SynTheme,
    conv: Box<dyn Fn((u8, u8, u8)) -> Color>,
    cache: HashMap<String, FileHl>,
}

#[cfg(feature = "syntax")]
impl Hl {
    pub fn new(theme_name: &str, conv: Box<dyn Fn((u8, u8, u8)) -> Color>) -> Self {
        // Leaked once per process: HighlightLines borrows both, and the cache outlives any scope.
        let ps: &'static SyntaxSet = Box::leak(Box::new(SyntaxSet::load_defaults_newlines()));
        let mut themes = ThemeSet::load_defaults().themes;
        let theme = themes.remove(theme_name).unwrap_or_default();
        Hl {
            ps,
            theme: Box::leak(Box::new(theme)),
            conv,
            cache: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// Highlights lines[..upto] of this file (idempotent, resumes where it stopped).
    pub fn ensure(&mut self, key: &str, path: &str, lines: &[DLine], upto: usize) {
        let (ps, theme) = (self.ps, self.theme);
        let f = self.cache.entry(key.to_string()).or_insert_with(|| {
            let ext = path.rsplit_once('.').map_or(path, |(_, e)| e);
            let found = ps.find_syntax_by_extension(ext);
            let syntax = found.unwrap_or_else(|| ps.find_syntax_plain_text());
            FileHl {
                syntax,
                plain: found.is_none(),
                done: 0,
                spans: vec![],
                old: HighlightLines::new(syntax, theme),
                new: HighlightLines::new(syntax, theme),
            }
        });
        if f.plain {
            return;
        }
        let upto = upto.min(lines.len());
        while f.done < upto {
            let d = &lines[f.done];
            let mut out: Spans = vec![];
            if d.op == Op::Hunk {
                // each hunk starts from a fresh parse state; the lines before it are unknown
                f.old = HighlightLines::new(f.syntax, theme);
                f.new = HighlightLines::new(f.syntax, theme);
            } else {
                let text = format!("{}\n", d.text);
                let feed = |h: &mut HighlightLines<'static>| {
                    h.highlight_line(&text, ps).unwrap_or_default()
                };
                let ranges = match d.op {
                    Op::Del => feed(&mut f.old),
                    Op::Add => feed(&mut f.new),
                    _ => {
                        feed(&mut f.old);
                        feed(&mut f.new)
                    }
                };
                for (st, s) in ranges {
                    let c = st.foreground;
                    out.push((
                        (self.conv)((c.r, c.g, c.b)),
                        s.trim_end_matches('\n').to_string(),
                    ));
                }
            }
            f.spans.push(out);
            f.done += 1;
        }
    }

    pub fn get(&self, key: &str, i: usize) -> Option<&Spans> {
        self.cache.get(key)?.spans.get(i)
    }
}

/// Without the `syntax` feature the diff view falls back to plain +/- coloring.
#[cfg(not(feature = "syntax"))]
pub struct Hl;

#[cfg(not(feature = "syntax"))]
impl Hl {
    pub fn new(_: &str, _: Box<dyn Fn((u8, u8, u8)) -> Color>) -> Self {
        Hl
    }
    pub fn clear(&mut self) {}
    pub fn ensure(&mut self, _: &str, _: &str, _: &[DLine], _: usize) {}
    pub fn get(&self, _: &str, _: usize) -> Option<&Spans> {
        None
    }
}

#[cfg(all(test, feature = "syntax"))]
mod tests {
    use super::*;
    use crate::diff;

    /// Perf guard: a 5k-line diff must highlight end-to-end quickly (run with --release).
    #[test]
    #[ignore]
    fn five_thousand_lines() {
        let mut d = String::from(
            "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1,5000 +1,5000 @@\n",
        );
        for i in 0..5000 {
            d += &format!("+let value_{i} = compute(\"s{i}\", {i}) + other::<u32>(); // note\n");
        }
        let files = diff::parse(&d);
        let mut hl = Hl::new(
            "base16-ocean.dark",
            Box::new(|(r, g, b)| Color::Rgb(r, g, b)),
        );
        let t = std::time::Instant::now();
        hl.ensure("k", "x.rs", &files[0].lines, usize::MAX);
        println!("5000 lines highlighted in {:?}", t.elapsed());
        assert!(hl.get("k", 100).is_some_and(|s| !s.is_empty()));
    }
}
