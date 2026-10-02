//! A small GitHub-flavored-markdown renderer for comments: styled, width-aware ratatui lines.
use crate::diff::wrap_ranges;
use crate::theme::Theme;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::prelude::*;

/// Comments taller than this are cut to `KEEP` lines until expanded.
const MAX_LINES: usize = 30;
const KEEP: usize = 10;
/// Whitespace-free runs longer than this (base64 blobs, minified data) are truncated.
const LONG_TOKEN: usize = 100;

/// Highlighter hook: (language, code lines) -> colored segments per line.
pub type HlFn<'a> = &'a dyn Fn(&str, &[String]) -> Option<Vec<Vec<(Color, String)>>>;

pub struct Opts<'a> {
    pub width: usize,
    pub expanded: bool,
    pub hl: Option<HlFn<'a>>,
}

pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    /// Something was collapsed (details block, long comment): Enter expands.
    pub folded: bool,
}

/// The part of a styled line covering chars [s, e), styles preserved (so word highlights survive wrapping).
pub fn slice_spans(spans: &[Span<'static>], s: usize, e: usize) -> Vec<Span<'static>> {
    let (mut out, mut off) = (vec![], 0);
    for sp in spans {
        let n = sp.content.chars().count();
        let (a, b) = (off.max(s), (off + n).min(e));
        if a < b {
            let text: String = sp.content.chars().skip(a - off).take(b - a).collect();
            out.push(Span::styled(text, sp.style));
        }
        off += n;
        if off >= e {
            break;
        }
    }
    out
}

/// Splits text into (is_url, piece) runs. A URL ends at whitespace or `<>"`; trailing punctuation and
/// unbalanced closing brackets are not part of it ("see https://x.io/a)." links https://x.io/a).
fn split_urls(t: &str) -> Vec<(bool, &str)> {
    let (mut out, mut rest) = (vec![], t);
    while let Some(i) = ["https://", "http://"]
        .iter()
        .filter_map(|p| rest.find(p))
        .min()
    {
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| c.is_whitespace() || "<>\"`".contains(c))
            .unwrap_or(tail.len());
        let mut url = &tail[..end];
        loop {
            let Some(last) = url.chars().last() else {
                break;
            };
            let unbalanced =
                |open: char, close: char| url.matches(close).count() > url.matches(open).count();
            let trim = match last {
                '.' | ',' | ';' | ':' | '!' | '?' | '\'' => true,
                ')' => unbalanced('(', ')'),
                ']' => unbalanced('[', ']'),
                '}' => unbalanced('{', '}'),
                _ => false,
            };
            if !trim {
                break;
            }
            url = &url[..url.len() - last.len_utf8()];
        }
        // "http://" alone is not a link
        if url.len() <= "http://".len() && url.ends_with("//") {
            out.push((false, &rest[..i + url.len().max(1)]));
            rest = &rest[i + url.len().max(1)..];
            continue;
        }
        if i > 0 {
            out.push((false, &rest[..i]));
        }
        out.push((true, url));
        rest = &rest[i + url.len()..];
    }
    if !rest.is_empty() || out.is_empty() {
        out.push((false, rest));
    }
    out
}

fn strip_comments(src: &str) -> String {
    let (mut out, mut rest) = (String::new(), src);
    while let Some(i) = rest.find("<!--") {
        out.push_str(&rest[..i]);
        match rest[i..].find("-->") {
            Some(j) => rest = &rest[i + j + 3..],
            None => return out,
        }
    }
    out + rest
}

/// Collapse (or open) `<details>` blocks; returns the text and whether any block was collapsed.
fn fold_details(src: &str, expanded: bool) -> (String, bool) {
    let (mut out, mut folded) = (String::new(), false);
    let mut rest = src;
    loop {
        let low = rest.to_lowercase();
        let Some(i) = low.find("<details") else { break };
        let Some(open_end) = low[i..].find('>').map(|k| i + k + 1) else {
            break;
        };
        let (body, after) = match low[open_end..].find("</details>") {
            Some(j) => (
                &rest[open_end..open_end + j],
                &rest[open_end + j + "</details>".len()..],
            ),
            None => (&rest[open_end..], ""),
        };
        let bl = body.to_lowercase();
        let (summary, inner) = match (bl.find("<summary>"), bl.find("</summary>")) {
            (Some(a), Some(b)) if b > a => (body[a + 9..b].trim(), body[b + 10..].trim()),
            _ => ("details", body.trim()),
        };
        let summary = strip_tags(summary);
        out.push_str(&rest[..i]);
        if expanded {
            out.push_str(&format!("\n\n**\u{25be} {summary}**\n\n{inner}\n\n"));
        } else {
            folded = true;
            let n = inner.lines().count();
            out.push_str(&format!("\n\n*\u{25b8} {summary} ({n} lines hidden)*\n\n"));
        }
        rest = after;
    }
    out.push_str(rest);
    (out, folded)
}

fn strip_tags(s: &str) -> String {
    let (mut out, mut in_tag) = (String::new(), false);
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn clip(s: &str, w: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let (mut out, mut used) = (String::new(), 0);
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw > w {
            if w > 0 {
                out.pop();
                out.push('\u{2026}');
            }
            break;
        }
        used += cw;
        out.push(c);
    }
    out
}

enum Ctx {
    Quote,
    List(Option<u64>),
    Item { marker: String, used: bool },
}

struct Table {
    rows: Vec<Vec<String>>,
    head_rows: usize,
    cell: Option<String>,
    row: Vec<String>,
    in_head: bool,
}

struct R<'a> {
    th: &'a Theme,
    o: &'a Opts<'a>,
    lines: Vec<Line<'static>>,
    cur: Vec<Span<'static>>,
    styles: Vec<Style>,
    ctx: Vec<Ctx>,
    links: Vec<String>,
    link_text: Vec<String>,
    code: Option<(String, String)>,
    table: Option<Table>,
    /// Adjacent Text events, joined so a URL the parser split in pieces is still one link.
    pend: String,
}

impl R<'_> {
    fn style(&self) -> Style {
        self.styles.iter().fold(Style::new(), |a, s| a.patch(*s))
    }

    fn marker_width(m: &str) -> usize {
        Span::raw(m.to_string()).width()
    }

    /// (first-line prefix, continuation prefix) from the enclosing quotes and list items.
    fn prefixes(&mut self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let (mut first, mut rest) = (vec![], vec![]);
        let last_item = self.ctx.iter().rposition(|c| matches!(c, Ctx::Item { .. }));
        for (i, c) in self.ctx.iter_mut().enumerate() {
            match c {
                Ctx::Quote => {
                    let bar = Span::styled("\u{258f} ", Style::new().fg(self.th.quote));
                    first.push(bar.clone());
                    rest.push(bar);
                }
                Ctx::Item { marker, used } => {
                    let pad = " ".repeat(Self::marker_width(marker));
                    if !*used && Some(i) == last_item {
                        first.push(Span::styled(
                            marker.clone(),
                            Style::new().fg(self.th.accent),
                        ));
                    } else {
                        first.push(Span::raw(pad.clone()));
                    }
                    rest.push(Span::raw(pad));
                    *used = true;
                }
                Ctx::List(_) => {}
            }
        }
        (first, rest)
    }

    fn gap(&mut self) {
        if self.lines.last().is_some_and(|l| !l.spans.is_empty())
            && self.ctx.iter().all(|c| !matches!(c, Ctx::Item { .. }))
        {
            self.lines.push(Line::raw(""));
        }
    }

    fn flush(&mut self) {
        if self.cur.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.cur);
        let (first, rest) = self.prefixes();
        let pw = Line::from(first.clone())
            .width()
            .max(Line::from(rest.clone()).width());
        let avail = self.o.width.saturating_sub(pw).max(8);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        for (k, (a, b)) in wrap_ranges(&text, avail).into_iter().enumerate() {
            let mut l = if k == 0 { first.clone() } else { rest.clone() };
            // a row that broke after a space shouldn't end in it
            let b = a + text
                .chars()
                .skip(a)
                .take(b - a)
                .collect::<String>()
                .trim_end()
                .chars()
                .count();
            l.extend(slice_spans(&spans, a, b));
            self.lines.push(Line::from(l));
        }
    }

    fn text(&mut self, t: &str) {
        if let Some((_, code)) = &mut self.code {
            code.push_str(t);
            return;
        }
        if let Some(cell) = self.table.as_mut().and_then(|tb| tb.cell.as_mut()) {
            cell.push_str(t);
            return;
        }
        if let Some(lt) = self.link_text.last_mut() {
            lt.push_str(t);
        }
        let st = self.style();
        // bare URLs become links (unless this text is already inside an explicit link)
        let segs = if self.links.is_empty() {
            split_urls(t)
        } else {
            vec![(false, t)]
        };
        for (is_url, seg) in segs {
            if is_url {
                let ls = st.patch(Style::new().fg(self.th.link).underlined());
                self.cur.push(Span::styled(seg.to_string(), ls));
                continue;
            }
            let shortened: Vec<String> = seg
                .split(' ')
                .map(|w| {
                    let n = w.chars().count();
                    if n > LONG_TOKEN {
                        format!(
                            "{}\u{2026}(+{} chars)",
                            w.chars().take(48).collect::<String>(),
                            n - 48
                        )
                    } else {
                        w.to_string()
                    }
                })
                .collect();
            self.cur.push(Span::styled(shortened.join(" "), st));
        }
    }

    fn code_block(&mut self, lang: &str, code: &str) {
        self.gap();
        let th = self.th;
        let (_, rest) = self.prefixes();
        let pw = Line::from(rest.clone()).width();
        let w = self.o.width.saturating_sub(pw).max(8);
        let src: Vec<String> = code
            .trim_end_matches('\n')
            .lines()
            .map(|l| l.replace('\t', "    "))
            .collect();
        let colored = self.o.hl.and_then(|h| h(lang, &src));
        let bg = Style::new().bg(th.code_bg);
        for (i, l) in src.iter().enumerate() {
            let mut v = rest.clone();
            let mut used = 0;
            let segs = colored
                .as_ref()
                .and_then(|c| c.get(i))
                .filter(|s| !s.is_empty());
            match segs {
                Some(segs) => {
                    for (c, s) in segs {
                        let s = clip(s, w.saturating_sub(used));
                        used += Span::raw(s.clone()).width();
                        v.push(Span::styled(s, bg.fg(*c)));
                    }
                }
                None => {
                    let s = clip(l, w);
                    used = Span::raw(s.clone()).width();
                    v.push(Span::styled(s, bg.fg(th.text)));
                }
            }
            v.push(Span::styled(" ".repeat(w.saturating_sub(used)), bg));
            self.lines.push(Line::from(v));
        }
    }

    fn emit_table(&mut self, t: Table) {
        self.gap();
        let th = self.th;
        let cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return;
        }
        let (_, rest) = self.prefixes();
        let avail = self
            .o
            .width
            .saturating_sub(Line::from(rest.clone()).width())
            .max(cols * 4);
        let mut w: Vec<usize> = (0..cols)
            .map(|c| {
                t.rows
                    .iter()
                    .filter_map(|r| r.get(c))
                    .map(|s| Span::raw(s.clone()).width())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let seps = 3 * (cols - 1);
        while w.iter().sum::<usize>() + seps > avail {
            let (i, m) = w
                .iter()
                .enumerate()
                .max_by_key(|(_, v)| **v)
                .map(|(i, v)| (i, *v))
                .unwrap();
            if m <= 4 {
                break;
            }
            w[i] = m - 1;
        }
        let muted = Style::new().fg(th.muted);
        let mut out = vec![];
        for (ri, row) in t.rows.iter().enumerate() {
            // cells wrap onto extra lines instead of being cut off
            let cells: Vec<Vec<String>> = w
                .iter()
                .enumerate()
                .map(|(c, cw)| {
                    let text = row.get(c).map_or("", String::as_str);
                    wrap_ranges(text, *cw)
                        .into_iter()
                        .map(|(a, b)| {
                            text.chars()
                                .skip(a)
                                .take(b - a)
                                .collect::<String>()
                                .trim_end()
                                .to_string()
                        })
                        .collect()
                })
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(1);
            let st = if ri < t.head_rows {
                Style::new().bold()
            } else {
                Style::new()
            };
            for k in 0..height {
                let mut v = rest.clone();
                for (c, cw) in w.iter().enumerate() {
                    let cell = cells[c].get(k).cloned().unwrap_or_default();
                    let pad = cw.saturating_sub(Span::raw(cell.clone()).width());
                    v.push(Span::styled(format!("{cell}{}", " ".repeat(pad)), st));
                    if c + 1 < cols {
                        v.push(Span::styled(" \u{2502} ", muted));
                    }
                }
                out.push(Line::from(v));
            }
            if ri + 1 == t.head_rows {
                let rule = w
                    .iter()
                    .map(|n| "\u{2500}".repeat(*n))
                    .collect::<Vec<_>>()
                    .join("\u{2500}\u{253c}\u{2500}");
                let mut v = rest.clone();
                v.push(Span::styled(rule, muted));
                out.push(Line::from(v));
            }
        }
        self.lines.extend(out);
    }

    fn html(&mut self, h: &str) {
        let low = h.trim().to_lowercase();
        if low.starts_with("<br") {
            self.flush();
            return;
        }
        if low.starts_with("<img") {
            self.cur
                .push(Span::styled("[image]", Style::new().fg(self.th.muted)));
            return;
        }
        let t = strip_tags(h);
        if t.trim().is_empty() {
            return;
        }
        for (i, line) in t.trim_matches('\n').split('\n').enumerate() {
            if i > 0 {
                self.flush();
            }
            if !line.trim().is_empty() {
                self.text(line);
            }
        }
    }

    fn flush_text(&mut self) {
        if !self.pend.is_empty() {
            let t = std::mem::take(&mut self.pend);
            self.text(&t);
        }
    }

    fn event(&mut self, ev: Event<'_>) {
        let th = self.th;
        if !matches!(ev, Event::Text(_)) {
            self.flush_text();
        }
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => self.gap(),
                Tag::Heading { .. } => {
                    self.gap();
                    self.styles.push(Style::new().fg(th.accent).bold());
                }
                Tag::BlockQuote(_) => {
                    self.gap();
                    self.ctx.push(Ctx::Quote);
                    self.styles.push(Style::new().fg(th.quote).italic());
                }
                Tag::CodeBlock(kind) => {
                    let lang = match kind {
                        CodeBlockKind::Fenced(l) => {
                            l.split_whitespace().next().unwrap_or("").to_string()
                        }
                        CodeBlockKind::Indented => String::new(),
                    };
                    self.code = Some((lang, String::new()));
                }
                Tag::List(first) => {
                    if self.ctx.is_empty() {
                        self.gap();
                    }
                    self.ctx.push(Ctx::List(first));
                }
                Tag::Item => {
                    self.flush();
                    let marker = match self
                        .ctx
                        .iter_mut()
                        .rev()
                        .find_map(|c| if let Ctx::List(n) = c { Some(n) } else { None })
                    {
                        Some(Some(n)) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        _ => if th.ascii { "- " } else { "\u{2022} " }.to_string(),
                    };
                    self.ctx.push(Ctx::Item {
                        marker,
                        used: false,
                    });
                }
                Tag::Emphasis => self.styles.push(Style::new().italic()),
                Tag::Strong => self.styles.push(Style::new().bold()),
                Tag::Strikethrough => self.styles.push(Style::new().crossed_out()),
                Tag::Link { dest_url, .. } => {
                    self.links.push(dest_url.to_string());
                    self.link_text.push(String::new());
                    self.styles.push(Style::new().fg(th.link).underlined());
                }
                Tag::Image { .. } => self
                    .cur
                    .push(Span::styled("[image: ", Style::new().fg(th.muted))),
                Tag::Table(_) => {
                    self.table = Some(Table {
                        rows: vec![],
                        head_rows: 0,
                        cell: None,
                        row: vec![],
                        in_head: false,
                    });
                }
                Tag::TableHead => {
                    if let Some(t) = &mut self.table {
                        t.in_head = true;
                    }
                }
                Tag::TableRow => {}
                Tag::TableCell => {
                    if let Some(t) = &mut self.table {
                        t.cell = Some(String::new());
                    }
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => self.flush(),
                TagEnd::Heading(_) => {
                    self.flush();
                    self.styles.pop();
                }
                TagEnd::BlockQuote(_) => {
                    self.flush();
                    self.ctx.pop();
                    self.styles.pop();
                }
                TagEnd::CodeBlock => {
                    if let Some((lang, code)) = self.code.take() {
                        self.code_block(&lang, &code);
                    }
                }
                TagEnd::List(_) => {
                    self.flush();
                    self.ctx.pop();
                }
                TagEnd::Item => {
                    self.flush();
                    self.ctx.pop();
                }
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                    self.styles.pop();
                }
                TagEnd::Link => {
                    self.styles.pop();
                    let url = self.links.pop().unwrap_or_default();
                    let text = self.link_text.pop().unwrap_or_default();
                    if !url.is_empty() && url != text && self.table.is_none() {
                        self.cur
                            .push(Span::styled(format!(" ({url})"), Style::new().fg(th.muted)));
                    }
                }
                TagEnd::Image => self.cur.push(Span::styled("]", Style::new().fg(th.muted))),
                TagEnd::TableCell => {
                    if let Some(t) = &mut self.table
                        && let Some(c) = t.cell.take()
                    {
                        t.row.push(c.trim().to_string());
                    }
                }
                TagEnd::TableHead | TagEnd::TableRow => {
                    if let Some(t) = &mut self.table {
                        let row = std::mem::take(&mut t.row);
                        t.rows.push(row);
                        if matches!(tag, TagEnd::TableHead) {
                            t.in_head = false;
                            t.head_rows = t.rows.len();
                        }
                    }
                }
                TagEnd::Table => {
                    if let Some(t) = self.table.take() {
                        self.emit_table(t);
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if self.code.is_some() || self.table.as_ref().is_some_and(|tb| tb.cell.is_some()) {
                    self.text(&t);
                } else {
                    self.pend.push_str(&t);
                }
            }
            Event::Code(c) => {
                if let Some(cell) = self.table.as_mut().and_then(|tb| tb.cell.as_mut()) {
                    cell.push_str(&c);
                } else {
                    let st = self.style().patch(Style::new().fg(th.warn).bg(th.code_bg));
                    self.cur.push(Span::styled(format!(" {c} "), st));
                }
            }
            // GitHub comments keep the author's line breaks
            Event::SoftBreak | Event::HardBreak => self.flush(),
            Event::Rule => {
                self.gap();
                let (_, rest) = self.prefixes();
                let w = self
                    .o
                    .width
                    .saturating_sub(Line::from(rest.clone()).width());
                let mut v = rest;
                v.push(Span::styled(
                    th.border.horizontal_top.repeat(w),
                    Style::new().fg(th.muted),
                ));
                self.lines.push(Line::from(v));
            }
            Event::Html(h) | Event::InlineHtml(h) => self.html(&h),
            Event::TaskListMarker(done) => {
                let m = match (done, th.ascii) {
                    (true, false) => "\u{2611} ",
                    (false, false) => "\u{2610} ",
                    (true, true) => "[x] ",
                    (false, true) => "[ ] ",
                };
                // the checkbox replaces the bullet instead of sitting next to it
                if let Some(Ctx::Item { marker, .. }) = self
                    .ctx
                    .iter_mut()
                    .rev()
                    .find(|c| matches!(c, Ctx::Item { .. }))
                {
                    *marker = m.to_string();
                }
            }
            _ => {}
        }
    }
}

pub fn render(src: &str, th: &Theme, o: &Opts) -> Rendered {
    let (text, mut folded) = fold_details(&strip_comments(src), o.expanded);
    let mut r = R {
        th,
        o,
        lines: vec![],
        cur: vec![],
        styles: vec![],
        ctx: vec![],
        links: vec![],
        link_text: vec![],
        code: None,
        table: None,
        pend: String::new(),
    };
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for ev in Parser::new_ext(&text, opts) {
        r.event(ev);
    }
    r.flush_text();
    r.flush();
    while r.lines.last().is_some_and(|l| l.spans.is_empty()) {
        r.lines.pop();
    }
    let mut lines = r.lines;
    if !o.expanded && lines.len() > MAX_LINES {
        let more = lines.len() - KEEP;
        lines.truncate(KEEP);
        lines.push(Line::styled(
            format!("\u{2026} {more} more lines (Enter to expand)"),
            Style::new().fg(th.muted).italic(),
        ));
        folded = true;
    }
    Rendered { lines, folded }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::IconSet;

    fn th() -> Theme {
        Theme::new(false, IconSet::Unicode, true)
    }

    fn lines(src: &str, w: usize) -> Vec<String> {
        text(&render(
            src,
            &th(),
            &Opts {
                width: w,
                expanded: false,
                hl: None,
            },
        ))
    }

    fn text(r: &Rendered) -> Vec<String> {
        r.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn styled(src: &str) -> Rendered {
        render(
            src,
            &th(),
            &Opts {
                width: 60,
                expanded: false,
                hl: None,
            },
        )
    }

    fn span_with<'a>(r: &'a Rendered, needle: &str) -> &'a Span<'static> {
        r.lines
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content.contains(needle))
            .unwrap_or_else(|| panic!("no span {needle:?}"))
    }

    #[test]
    fn headings_emphasis_code_and_strikethrough() {
        let r = styled("# Title\n\nsome **bold** and *ital* and ~~gone~~ and `code` here\n");
        let t = th();
        assert_eq!(text(&r)[0], "Title");
        assert!(
            span_with(&r, "Title")
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(span_with(&r, "Title").style.fg, Some(t.accent));
        assert!(
            span_with(&r, "bold")
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            span_with(&r, "ital")
                .style
                .add_modifier
                .contains(Modifier::ITALIC)
        );
        assert!(
            span_with(&r, "gone")
                .style
                .add_modifier
                .contains(Modifier::CROSSED_OUT)
        );
        assert_eq!(span_with(&r, "code").style.bg, Some(t.code_bg));
        assert_eq!(text(&r)[1], "", "blank line between blocks");
    }

    #[test]
    fn lists_numbers_tasks_and_nesting() {
        let l = lines(
            "- one\n- two\n  - nested\n\n3. three\n4. four\n\n- [x] done\n- [ ] todo\n",
            40,
        );
        assert_eq!(l[0], "\u{2022} one");
        assert_eq!(l[1], "\u{2022} two");
        assert_eq!(l[2], "  \u{2022} nested");
        assert!(l.contains(&"3. three".to_string()) && l.contains(&"4. four".to_string()));
        assert!(
            l.contains(&"\u{2611} done".to_string()) && l.contains(&"\u{2610} todo".to_string()),
            "{l:?}"
        );
    }

    #[test]
    fn wrapped_list_item_keeps_hanging_indent() {
        let l = lines("- alpha beta gamma delta epsilon\n", 14);
        assert_eq!(l, ["\u{2022} alpha beta", "  gamma delta", "  epsilon"]);
    }

    #[test]
    fn blockquote_links_images_and_rules() {
        let l = lines(
            "> quoted text\n> more\n\n[gh](https://example.com) and <https://auto.link>\n\n---\n\n![logo](x.png)\n",
            50,
        );
        assert_eq!(l[0], "\u{258f} quoted text");
        assert_eq!(l[1], "\u{258f} more");
        assert!(
            l.contains(&"gh (https://example.com) and https://auto.link".to_string()),
            "{l:?}"
        );
        assert!(
            l.iter()
                .any(|x| x.starts_with("\u{2500}") && x.chars().count() == 50)
        );
        assert!(l.contains(&"[image: logo]".to_string()), "{l:?}");
    }

    #[test]
    fn fenced_code_is_padded_dimmed_and_highlightable() {
        let t = th();
        let r = styled("```rust\nfn main() {}\n```\n");
        assert_eq!(text(&r), ["fn main() {}".to_string() + &" ".repeat(48)]);
        assert!(
            r.lines[0]
                .spans
                .iter()
                .all(|s| s.style.bg == Some(t.code_bg))
        );
        let hl: HlFn = &|lang, code| {
            assert_eq!(lang, "rust");
            Some(code.iter().map(|l| vec![(Color::Red, l.clone())]).collect())
        };
        let r = render(
            "```rust\nlet x = 1;\n```\n",
            &t,
            &Opts {
                width: 30,
                expanded: false,
                hl: Some(hl),
            },
        );
        assert_eq!(r.lines[0].spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn tables_align_and_shrink() {
        let l = lines("| name | n |\n|---|---|\n| alpha | 1 |\n| b | 22 |\n", 40);
        assert_eq!(l[0], "name  \u{2502} n ");
        assert!(l[1].starts_with("\u{2500}") && l[1].contains('\u{253c}'));
        assert_eq!(l[2], "alpha \u{2502} 1 ");
        let l = lines(
            "| a | b |\n|---|---|\n| aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa | bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb |\n",
            24,
        );
        assert!(l.iter().all(|x| x.chars().count() <= 24), "{l:?}");
    }

    #[test]
    fn html_comments_details_and_boilerplate_collapse() {
        let src = "hi <!-- hidden bot marker -->there\n\n<details>\n<summary>Show logs</summary>\n\nline a\n\nline b\n</details>\n";
        let r = render(
            src,
            &th(),
            &Opts {
                width: 50,
                expanded: false,
                hl: None,
            },
        );
        let t = text(&r).join("\n");
        assert!(t.contains("hi there") && !t.contains("hidden bot"), "{t}");
        assert!(
            t.contains("\u{25b8} Show logs") && t.contains("hidden") && !t.contains("line a"),
            "{t}"
        );
        assert!(r.folded);
        let r = render(
            src,
            &th(),
            &Opts {
                width: 50,
                expanded: true,
                hl: None,
            },
        );
        let t = text(&r).join("\n");
        assert!(
            t.contains("\u{25be} Show logs") && t.contains("line a") && t.contains("line b"),
            "{t}"
        );
        assert!(!r.folded);
    }

    #[test]
    fn long_tokens_and_long_comments_truncate_until_expanded() {
        let blob = "A".repeat(500);
        let l = lines(&format!("token: {blob} end"), 40);
        assert!(
            l.join(" ").contains("(+452 chars)") && l.iter().all(|x| x.chars().count() <= 40),
            "{l:?}"
        );
        let many = (0..60).map(|i| format!("para {i}\n\n")).collect::<String>();
        let r = render(
            &many,
            &th(),
            &Opts {
                width: 40,
                expanded: false,
                hl: None,
            },
        );
        assert_eq!(r.lines.len(), KEEP + 1);
        assert!(
            text(&r)
                .last()
                .unwrap()
                .contains("more lines (Enter to expand)")
                && r.folded
        );
        let r = render(
            &many,
            &th(),
            &Opts {
                width: 40,
                expanded: true,
                hl: None,
            },
        );
        assert!(r.lines.len() > 60 && !r.folded);
    }

    #[test]
    fn wide_chars_wrap_within_width_and_breaks_are_kept() {
        let l = lines(
            "\u{65e5}\u{672c}\u{8a9e}\u{65e5}\u{672c}\u{8a9e}\u{65e5}\u{672c}\u{8a9e}\n",
            8,
        );
        assert!(
            l.iter().all(|x| Span::raw(x.clone()).width() <= 8) && l.len() >= 3,
            "{l:?}"
        );
        assert_eq!(lines("one\ntwo<br>three\n", 20), ["one", "two", "three"]);
    }

    /// Regression (text loss at some widths): every word of the source must survive wrapping,
    /// at every width, through quotes, lists, tables, inline code and long tokens.
    #[test]
    fn no_word_is_lost_at_any_width() {
        let src = "> Acceptance tests currently require a manually supplied user token and are not available as a repository workflow. This adds a manually dispatched workflow that mints a GitHub App installation token for the dedicated `gh-acceptance-testing` organization and runs on Linux, macOS, Windows, or a selected operating system.\n>\n> Each testscript now declares whether it requires a token that authenticates a human user, and the **bold words** and *italic words* too.\n\n- first item with a [link text](https://example.com/a/very/long/path/that/keeps/going) inside it\n  - nested item that is also fairly long so that it needs to wrap around\n- [x] done task with `code spans` and more words after them\n\n| column one | column two |\n|---|---|\n| short | some longer cell content here |\n\n1. numbered long item that wraps around the available width several times over\n\nSupercalifragilisticexpialidocious_and_more-averyveryverylongunbrokentokenwithoutspaces end.\n\n```\ncode line that is long enough to be clipped by the narrow widths used here\n```\n";
        let words = [
            "Acceptance",
            "currently",
            "manually",
            "supplied",
            "repository",
            "workflow",
            "dispatched",
            "installation",
            "dedicated",
            "gh-acceptance-testing",
            "organization",
            "Windows",
            "selected",
            "operating",
            "system",
            "Each",
            "testscript",
            "declares",
            "whether",
            "requires",
            "authenticates",
            "human",
            "bold",
            "words",
            "italic",
            "first",
            "item",
            "link",
            "text",
            "inside",
            "nested",
            "fairly",
            "wrap",
            "around",
            "done",
            "task",
            "code",
            "spans",
            "more",
            "after",
            "them",
            "numbered",
            "available",
            "several",
            "times",
            "Supercalifragilisticexpialidocious_and_more-averyveryverylong",
        ];
        for w in 12..=200 {
            let r = render(
                src,
                &th(),
                &Opts {
                    width: w,
                    expanded: true,
                    hl: None,
                },
            );
            let t = text(&r);
            assert!(
                t.iter().all(|l| Span::raw(l.clone()).width() <= w),
                "width {w}: a row is wider than the pane\n{t:#?}"
            );
            let flat: String = t
                .concat()
                .chars()
                .filter(|c| !c.is_whitespace() && !"\u{258f}\u{2022}".contains(*c))
                .collect();
            for word in words {
                let want: String = word.chars().filter(|c| !c.is_whitespace()).collect();
                assert!(flat.contains(&want), "width {w}: lost {word:?}\n{t:#?}");
            }
        }
    }

    #[test]
    fn bare_urls_become_links_without_trailing_punctuation() {
        let t = th();
        let all = "see https://example.com/a/b?x=1, and (https://en.wikipedia.org/wiki/Rust_(language)). Also http://plain.io!";
        let r = render(
            all,
            &t,
            &Opts {
                width: 200,
                expanded: false,
                hl: None,
            },
        );
        assert_eq!(text(&r)[0], all);
        for url in [
            "https://example.com/a/b?x=1",
            "https://en.wikipedia.org/wiki/Rust_(language)",
            "http://plain.io",
        ] {
            let sp = span_with(&r, url);
            assert_eq!(
                sp.content, url,
                "trailing punctuation stays out of the link"
            );
            assert_eq!(
                (
                    sp.style.fg,
                    sp.style.add_modifier.contains(Modifier::UNDERLINED)
                ),
                (Some(t.link), true)
            );
        }
        // inside code and explicit links nothing is added; a long URL is never truncated
        let r = styled("`https://in.code/x` and [txt](https://t.co/y)");
        assert!(
            !r.lines[0]
                .spans
                .iter()
                .any(|s| s.content == "https://in.code/x"
                    && s.style.add_modifier.contains(Modifier::UNDERLINED))
        );
        assert!(text(&r)[0].contains("txt (https://t.co/y)"));
        let long = format!("https://example.com/{}", "a/".repeat(80));
        let l = lines(&format!("go {long} now"), 40);
        assert!(l.concat().replace(' ', "").contains(&long), "{l:?}");
        assert!(
            split_urls("no links http:// here").iter().all(|(u, _)| !u),
            "a lone scheme stays text"
        );
    }
}
