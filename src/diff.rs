use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Ctx,
    Add,
    Del,
    Hunk,
}

#[derive(Clone, Debug)]
pub struct DLine {
    pub op: Op,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

pub struct File {
    /// Display path (look-alike characters neutralized).
    pub path: String,
    /// The real path, for API calls such as inline comments.
    pub raw_path: String,
    /// M(odified), A(dded), D(eleted) or R(enamed).
    pub status: char,
    pub adds: u32,
    pub dels: u32,
    /// For changed lines: the removed/added line they pair with (word-level highlight).
    pub partner: Vec<Option<usize>>,
    note: Option<String>,
    pub lines: Vec<DLine>,
}

impl DLine {
    /// (line, side) a review comment on this line attaches to.
    pub fn anchor(&self) -> Option<(u32, &'static str)> {
        match self.op {
            Op::Hunk => None,
            Op::Del => self.old.map(|n| (n, "LEFT")),
            _ => self.new.map(|n| (n, "RIGHT")),
        }
    }
}

/// One side-by-side row: a full-width hunk header or a left/right pair.
/// Rows hold indices into the file's lines (no cloning).
pub enum Row {
    Full(usize),
    Pair(Option<usize>, Option<usize>),
}

pub fn parse(diff: &str) -> Vec<File> {
    let mut files: Vec<File> = vec![];
    let (mut old, mut new, mut in_hunk) = (0u32, 0u32, false);
    for l in diff.lines() {
        if let Some(rest) = l.strip_prefix("diff --git ") {
            // Fallback only: ambiguous when the path contains " b/"; ---/+++ lines override it.
            let path = rest.rsplit_once(" b/").map_or(rest, |(_, p)| p);
            files.push(File {
                path: crate::sanitize::clean(path).into_owned(),
                raw_path: path.to_string(),
                lines: vec![],
                note: None,
                status: 'M',
                adds: 0,
                dels: 0,
                partner: vec![],
            });
            in_hunk = false;
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if let Some(h) = l.strip_prefix("@@ ") {
            let num = |p: Option<&str>| {
                p.and_then(|p| {
                    p.trim_start_matches(['-', '+'])
                        .split(',')
                        .next()?
                        .parse()
                        .ok()
                })
                .unwrap_or(0)
            };
            let mut it = h.split(' ');
            (old, new, in_hunk) = (num(it.next()), num(it.next()), true);
            f.lines.push(DLine {
                op: Op::Hunk,
                old: None,
                new: None,
                text: l.into(),
            });
            continue;
        }
        if l.starts_with("new file mode") && !in_hunk {
            f.status = 'A';
            continue;
        }
        if l.starts_with("deleted file mode") && !in_hunk {
            f.status = 'D';
            continue;
        }
        if let Some(p) = l.strip_prefix("--- ").filter(|_| !in_hunk) {
            match header_path(p, "a/") {
                Some(p) => {
                    f.path = crate::sanitize::clean(&p).into_owned();
                    f.raw_path = p;
                }
                None => f.status = 'A',
            }
            continue;
        }
        if let Some(p) = l.strip_prefix("+++ ").filter(|_| !in_hunk) {
            match header_path(p, "b/") {
                Some(p) => {
                    f.path = crate::sanitize::clean(&p).into_owned();
                    f.raw_path = p;
                }
                None => f.status = 'D',
            }
            continue;
        }
        if let Some(p) = l.strip_prefix("rename to ").filter(|_| !in_hunk) {
            f.status = 'R';
            f.note = Some(format!("renamed to {}", unquote(p)));
            f.raw_path = unquote(p);
            f.path = crate::sanitize::clean(&f.raw_path).into_owned();
            continue;
        }
        if l.starts_with("Binary files ") && !in_hunk {
            f.note = Some("binary file differs".into());
            continue;
        }
        // Without this, a deleted line like "-- x" ("--- x") would be mistaken for a file header.
        if !in_hunk {
            continue;
        }
        let (op, text) = match l.chars().next() {
            Some('+') => (Op::Add, &l[1..]),
            Some('-') => (Op::Del, &l[1..]),
            Some('\\') => continue,
            _ => (Op::Ctx, l.get(1..).unwrap_or("")),
        };
        let text = crate::sanitize::clean(&text.replace('\t', "    ")).into_owned();
        let (o, n) = match op {
            Op::Add => (None, Some(new)),
            Op::Del => (Some(old), None),
            _ => (Some(old), Some(new)),
        };
        old += u32::from(o.is_some());
        new += u32::from(n.is_some());
        f.lines.push(DLine {
            op,
            old: o,
            new: n,
            text,
        });
    }
    for f in &mut files {
        f.adds = f.lines.iter().filter(|l| l.op == Op::Add).count() as u32;
        f.dels = f.lines.iter().filter(|l| l.op == Op::Del).count() as u32;
        if f.lines.is_empty() {
            let text = f
                .note
                .take()
                .unwrap_or_else(|| "no text changes (mode change or empty file)".into());
            f.lines.push(DLine {
                op: Op::Hunk,
                old: None,
                new: None,
                text,
            });
        }
    }
    for f in &mut files {
        let mut p = vec![None; f.lines.len()];
        for r in split(&f.lines) {
            if let Row::Pair(Some(l), Some(r)) = r
                && l != r
            {
                (p[l], p[r]) = (Some(r), Some(l));
            }
        }
        f.partner = p;
    }
    files
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    Auto,
    Unified,
    Split,
}

impl DiffMode {
    pub fn next(self) -> Self {
        match self {
            DiffMode::Auto => DiffMode::Unified,
            DiffMode::Unified => DiffMode::Split,
            DiffMode::Split => DiffMode::Auto,
        }
    }
}

/// Side-by-side only when each half keeps at least 60 columns (else long lines get unreadable).
pub fn use_split(mode: DiffMode, width: usize) -> bool {
    match mode {
        DiffMode::Split => true,
        DiffMode::Unified => false,
        DiffMode::Auto => width.saturating_sub(1) / 2 >= 60,
    }
}

/// Chars after which a row may break (besides whitespace), so URLs and paths wrap at `/` or `-`.
const BREAK_AFTER: &str = "/-,;:)]}&?=|";

/// Char-index ranges, one per display row, so every row fits `width` terminal cells.
/// Rows break after the last whitespace/punctuation that fits; only a token longer than the
/// width is hard-broken. A double-width char that doesn't fit moves to the next row.
/// Always at least one range.
pub fn wrap_ranges(text: &str, width: usize) -> Vec<(usize, usize)> {
    let width = width.max(1);
    let ws: Vec<usize> = text.chars().map(|c| c.width().unwrap_or(0)).collect();
    let chars: Vec<char> = text.chars().collect();
    let (mut out, mut start, mut cur, mut brk) = (vec![], 0, 0, None);
    for n in 0..chars.len() {
        if cur + ws[n] > width && n > start {
            // break after the last opportunity in this row, else mid-token
            let at = brk.filter(|&b| b > start).unwrap_or(n);
            out.push((start, at));
            start = at;
            cur = ws[start..n].iter().sum();
            brk = None;
        }
        cur += ws[n];
        if chars[n].is_whitespace() || BREAK_AFTER.contains(chars[n]) {
            brk = Some(n + 1);
        }
    }
    out.push((start, chars.len()));
    out
}

/// Path from a `---`/`+++` line, None for /dev/null. Git quotes odd paths and tab-terminates spaced ones.
fn header_path(p: &str, prefix: &str) -> Option<String> {
    let p = unquote(p.trim_end_matches('\t'));
    (p != "/dev/null").then(|| p.strip_prefix(prefix).unwrap_or(&p).to_string())
}

/// Undo git's C-style quoting ("caf\303\251.txt").
fn unquote(s: &str) -> String {
    let Some(q) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return s.to_string();
    };
    let (mut out, mut it) = (Vec::new(), q.bytes().peekable());
    while let Some(b) = it.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match it.next() {
            Some(d @ b'0'..=b'7') => {
                let mut v = u32::from(d - b'0');
                for _ in 0..2 {
                    if let Some(d @ b'0'..=b'7') = it.peek().copied() {
                        v = v * 8 + u32::from(d - b'0');
                        it.next();
                    }
                }
                out.push(v as u8);
            }
            Some(b't') => out.push(b'\t'),
            Some(b'n') => out.push(b'\n'),
            Some(c) => out.push(c),
            None => {}
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn split(lines: &[DLine]) -> Vec<Row> {
    let mut rows = vec![];
    let mut i = 0;
    while i < lines.len() {
        match lines[i].op {
            Op::Hunk => {
                rows.push(Row::Full(i));
                i += 1;
            }
            Op::Ctx => {
                rows.push(Row::Pair(Some(i), Some(i)));
                i += 1;
            }
            _ => {
                let d0 = i;
                while i < lines.len() && lines[i].op == Op::Del {
                    i += 1;
                }
                let a0 = i;
                while i < lines.len() && lines[i].op == Op::Add {
                    i += 1;
                }
                for k in 0..(a0 - d0).max(i - a0) {
                    rows.push(Row::Pair((d0..a0).nth(k), (a0..i).nth(k)));
                }
            }
        }
    }
    rows
}

impl Row {
    pub fn anchor(&self, lines: &[DLine]) -> Option<(u32, &'static str)> {
        let at = |i: Option<usize>| i.and_then(|i| lines.get(i)?.anchor());
        match self {
            Row::Full(_) => None,
            Row::Pair(l, r) => at(*r).or(at(*l)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/a.txt b/a.txt\nindex 1..2 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@ fn x\n keep\n-old1\n-old2\n+new1\n tail\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -0,0 +1 @@\n+hello\n";

    #[test]
    fn parses_files_and_numbers() {
        let f = parse(DIFF);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "a.txt");
        let l = &f[0].lines; // hunk, keep, -old1, -old2, +new1, tail
        assert_eq!((l[1].old, l[1].new), (Some(1), Some(1)));
        assert_eq!((l[3].op, l[3].old, l[3].new), (Op::Del, Some(3), None));
        assert_eq!((l[4].op, l[4].old, l[4].new), (Op::Add, None, Some(2)));
        assert_eq!(l[5].new, Some(3));
        assert_eq!(f[1].lines[1].new, Some(1));
    }

    #[test]
    fn split_pairs_dels_with_adds() {
        let files = parse(DIFF);
        let rows = split(&files[0].lines);
        // hunk, keep, (old1|new1), (old2|-), tail
        assert_eq!(rows.len(), 5);
        assert!(matches!(&rows[0], Row::Full(_)));
        let Row::Pair(Some(d), Some(a)) = &rows[2] else {
            panic!()
        };
        let l = &files[0].lines;
        assert_eq!((l[*d].text.as_str(), l[*a].text.as_str()), ("old1", "new1"));
        assert!(matches!(&rows[3], Row::Pair(Some(_), None)));
        assert_eq!(rows[2].anchor(l), Some((2, "RIGHT")));
        assert_eq!(rows[3].anchor(l), Some((3, "LEFT")));
        assert_eq!(rows[0].anchor(l), None);
    }

    #[test]
    fn path_from_headers_not_the_ambiguous_git_line() {
        let d = "diff --git a/dir b/f.txt b/dir b/f.txt\n--- a/dir b/f.txt\t\n+++ b/dir b/f.txt\t\n@@ -1 +1 @@\n-a\n+b\n\
diff --git a/caf.txt b/caf.txt\n--- a/caf.txt\n+++ \"b/caf\\303\\251.txt\"\n@@ -1 +1 @@\n-a\n+b\n\
diff --git a/gone.txt b/gone.txt\n--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-a\n\
diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n";
        let f = parse(d);
        let paths: Vec<_> = f.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["dir b/f.txt", "café.txt", "gone.txt", "img.png"]);
        assert_eq!(f[3].lines[0].text, "binary file differs");
    }

    #[test]
    fn wrap_rows_markers_and_wide_chars() {
        assert_eq!(wrap_ranges("", 10), [(0, 0)]);
        // no break opportunity: hard-break at the width
        assert_eq!(wrap_ranges("abcdef", 3), [(0, 3), (3, 6)]);
        assert_eq!(wrap_ranges("abcdefg", 3), [(0, 3), (3, 6), (6, 7)]);
        // prefers the last space that fits
        assert_eq!(
            wrap_ranges("hello world foo", 8),
            [(0, 6), (6, 12), (12, 15)]
        );
        // a token longer than the row is hard-broken, the words around it still wrap at spaces
        assert_eq!(
            wrap_ranges("ab abcdefghij c", 5),
            [(0, 3), (3, 8), (8, 13), (13, 15)]
        );
        // each CJK char is 2 cells: width 5 fits two (4 cells), the third wraps
        assert_eq!(wrap_ranges("日本語日本", 5), [(0, 2), (2, 4), (4, 5)]);
        // a wide char never splits across rows even when only 1 cell is left
        assert_eq!(wrap_ranges("a日", 2), [(0, 1), (1, 2)]);
        // width 1 can't fit a wide char; it still gets its own row instead of looping
        assert_eq!(wrap_ranges("日日", 1), [(0, 1), (1, 2)]);
        // rows never exceed the width and cover the text exactly once
        let t =
            "see https://careers.example.com/job/1c25-8c14/senior-engineer now, thanks (really)";
        let r = wrap_ranges(t, 20);
        assert_eq!(r.first().unwrap().0, 0);
        assert_eq!(r.last().unwrap().1, t.chars().count());
        assert!(r.windows(2).all(|w| w[0].1 == w[1].0));
        assert!(r.iter().all(|&(a, b)| b - a <= 20));
        // URL breaks after path punctuation rather than mid-word
        let (a, b) = r[1];
        assert!(
            t.chars()
                .skip(a)
                .take(b - a)
                .collect::<String>()
                .ends_with(['/', '-', ' '])
        );
    }

    #[test]
    fn auto_layout_threshold() {
        assert!(!use_split(DiffMode::Auto, 120)); // halves of 59
        assert!(use_split(DiffMode::Auto, 121)); // halves of 60
        assert!(use_split(DiffMode::Split, 40));
        assert!(!use_split(DiffMode::Unified, 300));
        assert!(
            DiffMode::Auto.next() == DiffMode::Unified && DiffMode::Split.next() == DiffMode::Auto
        );
    }

    #[test]
    fn file_status_counts_and_partners() {
        let d = "diff --git a/n.txt b/n.txt\nnew file mode 100644\n--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1 @@\n+x\n\
diff --git a/g.txt b/g.txt\ndeleted file mode 100644\n--- a/g.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-y\n\
diff --git a/o.txt b/p.txt\nsimilarity index 100%\nrename from o.txt\nrename to p.txt\n";
        let f = parse(d);
        let st: Vec<_> = f.iter().map(|f| (f.status, f.adds, f.dels)).collect();
        assert_eq!(st, [('A', 1, 0), ('D', 0, 1), ('R', 0, 0)]);
        let m = parse(DIFF);
        assert_eq!(m[0].partner[2], Some(4)); // -old1 pairs with +new1
        assert_eq!(m[0].partner[4], Some(2));
        assert_eq!(m[0].partner[3], None);
    }

    #[test]
    fn display_path_is_neutralized_but_the_real_path_is_kept() {
        let f = parse(
            "diff --git a/x b/x\n--- a/we\u{200b}ird.rs\n+++ b/we\u{200b}ird.rs\n@@ -1 +1 @@\n-a\n+b\n",
        );
        assert_eq!(f[0].path, "we<U+200B>ird.rs");
        assert_eq!(f[0].raw_path, "we\u{200b}ird.rs");
    }
}
