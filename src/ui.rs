use crate::app::{App, Hit, Load, Modal, PK};
use crate::browse::{self, Browser};
use crate::config::Act;
use crate::diff::{self, DLine, DiffMode, Op, Row};
use crate::gh::{self, Data, FilesData, Item, Kind, LIMIT, Tab};
use crate::md::slice_spans;
use crate::syn::Hl;
use crate::theme::Theme;
use ratatui::{
    prelude::*,
    widgets::{Block, Clear, List, ListItem, ListState, Paragraph},
};

const MIN_W: u16 = 50;
const MIN_H: u16 = 12;

/// The `?` text. Remappable actions show whatever keys the active keymap gives them.
fn help_text(app: &App) -> String {
    let k = |a: Act| app.keys.labels(a);
    format!(
        "\
Panels (Status, Pull requests, Files, Issues, Actions, Branches, Releases, Notifications)
  1-8, Tab/S-Tab  focus panel      {{ }}  switch the panel's list tab (Mine/Review/...)
  j/k, arrows     move             Ctrl-d/u  half page     g/G or Home/End  top/bottom
  {filter}  filter (Enter apply, Esc clear)
  l/Right         focus the detail pane   h/Esc/Left  back to the list
  {global}  (PR/Issues/... list focus) toggle global view; in Files and PR drill-in it jumps to the last row
  {browser}  repo browser: all your repos (search, sort, favorite, hide, Enter switches)
Pull requests
  Files panel follows the selected PR; j/k there changes the file shown in the diff
  Enter on a PR drills in: Files / Checks / Comments of that PR (Esc returns)
  [ ]  detail tab (Overview/Checks/Comments/Diff); in a PR drill-in: next/prev panel
Diff
  j/k line   Ctrl-d/u half page   g/G first/last row   n/p file   t unified/split/auto
  w wrap/clip   v mark file viewed   {zoom} zoom
Comments
  j/k move by comment   Ctrl-d/u half page (inside a tall comment first)
  Enter  expand/collapse (long comments, <details>)   e  show who reacted
Actions (always asks to confirm, shows the exact command)
  {actions}  action menu for the selected item / row    {approve} approve  {comment} comment  {merge} merge
  Input popups: Enter newline, Ctrl-S submit, Esc cancel
Mouse: click panel/row/tab, wheel scrolls
Anywhere
  {open}  open in browser    {copy}  copy URL    {checkout}  checkout selected PR
  {refresh}  refresh panel      {refresh_all}  refresh all    {log}  command log
  {help}  this help    {quit}  quit
(remap the keys marked above in config.toml, see docs/configuration.md)",
        filter = k(Act::Filter),
        global = k(Act::Global),
        browser = k(Act::Browser),
        zoom = k(Act::Zoom),
        actions = k(Act::Actions),
        approve = k(Act::Approve),
        comment = k(Act::Comment),
        merge = k(Act::Merge),
        open = k(Act::Open),
        copy = k(Act::CopyUrl),
        checkout = k(Act::Checkout),
        refresh = k(Act::Refresh),
        refresh_all = k(Act::RefreshAll),
        log = k(Act::CommandLog),
        help = k(Act::Help),
        quit = k(Act::Quit),
    )
}

pub fn draw(f: &mut Frame, app: &App) {
    let th = &app.theme;
    let area = f.area();
    *app.hit.borrow_mut() = Hit::default();
    if area.width < MIN_W || area.height < MIN_H {
        let msg = format!(
            "Terminal too small\nneed {MIN_W}x{MIN_H}, have {}x{}",
            area.width, area.height
        );
        let r = area.centered(Constraint::Length(area.width), Constraint::Length(2));
        f.render_widget(
            Paragraph::new(msg)
                .alignment(Alignment::Center)
                .style(Style::new().fg(th.warn)),
            r,
        );
        return;
    }
    if let Some(b) = &app.browser {
        return browser_view(f, app, b, area);
    }
    let log_h = if app.show_log && area.height >= 24 {
        8
    } else {
        0
    };
    let [head, main, logs, bar] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(log_h),
        Constraint::Length(1),
    ])
    .areas(area);

    let loading = app.panels.iter().any(|p| p.loading);
    let title = match &app.ctx {
        Some(c) => format!(" {} › PR #{}  {}", app.repo, c.pr.number, c.pr.title),
        None => app.header.clone(),
    };
    let mut hdr = vec![Span::styled(title, Style::new().fg(th.accent).bold())];
    if app.global {
        hdr.push(Span::styled("  [global view]", Style::new().fg(th.warn)));
    }
    if loading {
        hdr.push(Span::styled(
            format!("  {}", spinner(app)),
            Style::new().fg(th.muted),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(hdr)), head);

    if app.show_log && log_h > 0 {
        let l = gh::cmd_log();
        let rows = logs.height.saturating_sub(2) as usize;
        let text: Vec<Line> = l[l.len().saturating_sub(rows)..]
            .iter()
            .map(|c| Line::styled(c.as_str(), Style::new().fg(th.muted)))
            .collect();
        f.render_widget(
            Paragraph::new(text).block(bordered(app, " Command log (L hides) ".into(), false)),
            logs,
        );
    }

    // `f` zoom hides the left column so the right pane gets the full width.
    let [left, right] = if app.zoom {
        [Rect::default(), main]
    } else {
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(main)
    };
    // Short terminals: unfocused panels shrink to a single borderless line.
    let n = app.panels.len() as u16;
    let compact = left.height < 3 * (n - 1) + 5;
    let rows = (0..app.panels.len()).map(|i| match (i == app.focus, compact) {
        (true, true) => Constraint::Min(4),
        (true, false) => Constraint::Min(5),
        (false, true) => Constraint::Length(1),
        (false, false) => Constraint::Length(3),
    });
    if !app.zoom {
        for (i, a) in Layout::vertical(rows).split(left).iter().enumerate() {
            app.hit.borrow_mut().panels.push(*a);
            panel(f, app, i, *a, compact);
        }
    }
    detail(f, app, right);

    f.render_widget(Paragraph::new(bar_line(app)), bar);

    modal(f, app);
    if app.help {
        help_popup(f, app, area);
    }
}

/// Help text wrapped to the popup width: headings bold, continuation lines indented under their line.
fn help_lines(app: &App, w: usize) -> Vec<Line<'static>> {
    let th = &app.theme;
    let mut out = vec![];
    for l in help_text(app).lines() {
        let indent = l.len() - l.trim_start().len();
        if indent == 0 {
            out.push(Line::styled(
                l.to_string(),
                Style::new().fg(th.accent).bold(),
            ));
            continue;
        }
        let pad = " ".repeat(indent);
        for (k, row) in wrap(l.trim_start(), w.saturating_sub(indent + 2))
            .into_iter()
            .enumerate()
        {
            let extra = if k == 0 { "" } else { "  " };
            out.push(Line::raw(format!("{pad}{extra}{row}")));
        }
    }
    out
}

/// Sized to the longest line (up to the terminal), wrapping or scrolling (j/k) when it doesn't fit.
fn help_popup(f: &mut Frame, app: &App, area: Rect) {
    let text = help_text(app);
    let longest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let w = (longest + 4).min(area.width.saturating_sub(2)).max(20);
    let inner_w = w.saturating_sub(2) as usize;
    let lines = help_lines(app, inner_w);
    let h = (lines.len() as u16 + 2)
        .min(area.height.saturating_sub(2))
        .max(3);
    let rows = h.saturating_sub(2) as usize;
    let max = lines.len().saturating_sub(rows);
    app.help_max.set(max);
    let top = app.help_scroll.min(max);
    let title = if max > 0 {
        " Keys (j/k scroll, any other key closes) "
    } else {
        " Keys "
    };
    let a = area.centered(Constraint::Length(w), Constraint::Length(h));
    f.render_widget(Clear, a);
    f.render_widget(
        Paragraph::new(lines)
            .scroll((top as u16, 0))
            .block(popup_block(app, title.into())),
        a,
    );
}

/// `M  name  dir/   +a -d ◆threads ✓` with the name kept whole and the directory squeezed first.
fn file_label(app: &App, it: &Item, w: usize) -> Line<'static> {
    use unicode_width::UnicodeWidthStr;
    let th = &app.theme;
    let (adds, dels, threads) = it.fm;
    let sc = match it.state.as_str() {
        "A" => th.ok,
        "D" => th.err,
        "R" => th.merged,
        _ => th.warn,
    };
    let viewed = app.viewed.contains(&(app.pr_key(), it.title.clone()));
    // Thread and viewed slots are fixed-width so the +N -M column never shifts between rows.
    let thread = if threads > 0 {
        format!(" {}{threads}", th.ic.thread)
    } else {
        String::new()
    };
    let tick = if viewed {
        format!(" {}", th.ic.viewed)
    } else {
        String::new()
    };
    let right = vec![
        Span::styled(format!(" +{adds}"), Style::new().fg(th.ok)),
        Span::styled(format!(" -{dels}"), Style::new().fg(th.err)),
        Span::styled(format!("{thread:<4}"), Style::new().fg(th.warn)),
        Span::styled(format!("{tick:<2}"), Style::new().fg(th.ok)),
    ];
    let rw = Line::from(right.clone()).width();
    let budget = w.saturating_sub(2 + rw + 1);
    let (dir, name) = it
        .title
        .rsplit_once('/')
        .map_or(("", it.title.as_str()), |(d, n)| (d, n));
    let name = mid_ellipsis(name, budget, th.ic.ell);
    let mut v = vec![
        Span::styled(format!("{} ", it.state), Style::new().fg(sc).bold()),
        Span::styled(name.clone(), Style::new().add_modifier(Modifier::BOLD)),
    ];
    if !dir.is_empty() && budget > name.width() + 4 {
        let d = mid_ellipsis(&format!("{dir}/"), budget - name.width() - 2, th.ic.ell);
        v.push(Span::styled(format!("  {d}"), Style::new().fg(th.muted)));
    }
    let used: usize = Line::from(v.clone()).width();
    v.push(Span::raw(" ".repeat(w.saturating_sub(used + rw))));
    v.extend(right);
    Line::from(v)
}

/// Full-screen repository browser.
fn browser_view(f: &mut Frame, app: &App, b: &Browser, area: Rect) {
    use ratatui::widgets::{Cell, Row as TRow, Table, TableState};
    let th = &app.theme;
    let cfg = &app.cfg.repos;
    let vis = b.visible(cfg);
    let [head, search, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    let more = if b.truncated { "+" } else { "" };
    let dir = if b.desc { th.ic.down } else { th.ic.up };
    let mut h = vec![
        Span::styled(" Repositories ", Style::new().fg(th.accent).bold()),
        Span::styled(
            format!("{} of {}{more}", vis.len(), b.rows.len()),
            Style::new().fg(th.muted),
        ),
    ];
    if b.loading {
        h.push(Span::styled(
            format!("  {} loading {}...", spinner(app), b.rows.len()),
            Style::new().fg(th.muted),
        ));
    }
    h.push(Span::styled(
        format!(
            "   sort: {} {dir}   type: {}   hidden: {}",
            b.sort.label(),
            b.ty.label(),
            if b.show_hidden { "shown" } else { "off" }
        ),
        Style::new().fg(th.muted),
    ));
    f.render_widget(Paragraph::new(Line::from(h)), head);

    let cursor = if b.typing { "_" } else { "" };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " / ",
                Style::new().fg(if b.typing { th.accent } else { th.muted }),
            ),
            Span::raw(format!("{}{cursor}", b.query)),
        ])),
        search,
    );

    let hint = if let Some(e) = &b.error {
        Line::styled(format!(" {} {e}", th.ic.fail), Style::new().fg(th.err))
    } else if !app.status.is_empty() {
        Line::styled(format!(" {}", app.status), Style::new().fg(th.warn))
    } else if b.typing {
        Line::styled(
            " type to search   Enter/Esc leave the box   Up/Down move",
            Style::new().fg(th.muted),
        )
    } else {
        Line::styled(
            " Enter switch  f favorite  H hide  . hidden  s sort  S direction  T type  / search  r reload  Esc back",
            Style::new().fg(th.muted),
        )
    };
    f.render_widget(Paragraph::new(hint), foot);

    if vis.is_empty() {
        let n = if b.loading {
            Note::Loading
        } else {
            Note::Empty("no repositories match")
        };
        return f.render_widget(Paragraph::new(note(app, n)), body);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let w = body.width;
    let (show_lang, show_counts) = (w >= 90, w >= 70);
    let mut widths = vec![
        Constraint::Length(2),
        Constraint::Min(16),
        Constraint::Length(4),
        Constraint::Length(2),
    ];
    if show_lang {
        widths.push(Constraint::Length(12));
    }
    widths.extend([Constraint::Length(6), Constraint::Length(5)]);
    if show_counts {
        widths.extend([Constraint::Length(4), Constraint::Length(4)]);
    }
    let muted = Style::new().fg(th.muted);
    let mut hdr = vec!["", "name", "vis", ""];
    if show_lang {
        hdr.push("language");
    }
    hdr.extend(["stars", "push"]);
    if show_counts {
        hdr.extend(["PR", "iss"]);
    }
    let header = TRow::new(hdr.into_iter().map(|c| Cell::from(Span::styled(c, muted))));
    let rows = vis.iter().map(|&i| {
        let r = &b.rows[i];
        let hidden = cfg.is_hidden(&r.name);
        let mark = if hidden {
            Span::styled(th.ic.hidden, muted)
        } else if cfg.is_fav(&r.name) {
            Span::styled(th.ic.fav, Style::new().fg(th.warn))
        } else {
            Span::raw(" ")
        };
        let base = if hidden { muted } else { Style::new() };
        let flags = format!(
            "{}{}",
            if r.fork { "F" } else { "" },
            if r.archived { "A" } else { "" }
        );
        let vis_c = if r.private {
            Span::styled("priv", Style::new().fg(th.warn))
        } else {
            Span::styled("pub", muted)
        };
        let mut cells = vec![
            Cell::from(mark),
            Cell::from(Span::styled(r.name.clone(), base)),
            Cell::from(vis_c),
            Cell::from(Span::styled(flags, muted)),
        ];
        if show_lang {
            cells.push(Cell::from(Span::styled(r.lang.clone(), muted)));
        }
        cells.push(Cell::from(Span::styled(r.stars.to_string(), base)));
        cells.push(Cell::from(Span::styled(browse::ago(&r.pushed, now), muted)));
        if show_counts {
            cells.push(Cell::from(Span::styled(r.prs.to_string(), base)));
            cells.push(Cell::from(Span::styled(r.issues.to_string(), base)));
        }
        TRow::new(cells)
    });
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(Style::new().bg(th.sel_bg).bold());
    let mut st = TableState::default().with_selected(Some(b.cursor.min(vis.len() - 1)));
    f.render_stateful_widget(table, body, &mut st);
}

fn spinner(app: &App) -> &'static str {
    let s = app.theme.ic.spin;
    s[app.tick / 2 % s.len()]
}

fn bar_line(app: &App) -> Line<'static> {
    let th = &app.theme;
    if app.typing {
        return Line::from(vec![
            Span::styled("/", Style::new().fg(th.accent)),
            Span::raw(format!("{}_", app.filter)),
        ]);
    }
    if !app.status.is_empty() {
        return Line::styled(app.status.clone(), Style::new().fg(th.warn));
    }
    let mut spans = vec![];
    for tok in hints(app).split("  ") {
        match tok.split_once(' ') {
            Some((k, d)) => {
                spans.push(Span::styled(k.to_string(), Style::new().fg(th.accent)));
                spans.push(Span::styled(format!(" {d}  "), Style::new().fg(th.muted)));
            }
            None => spans.push(Span::styled(format!("{tok}  "), Style::new().fg(th.muted))),
        }
    }
    Line::from(spans)
}

fn hints(app: &App) -> String {
    let kl = |a: Act| app.keys.label(a);
    let (actions, zoom, filter) = (kl(Act::Actions), kl(Act::Zoom), kl(Act::Filter));
    let flt = if app.filter.is_empty() {
        String::new()
    } else {
        format!("  [filter: {}]", app.filter)
    };
    let ctx = if app.zoom {
        format!("j/k move  t mode  w wrap  {zoom}/Esc unzoom  h back")
    } else if app.log.is_some() {
        "j/k scroll  Ctrl-d/u page  g/G ends  Esc close log".to_string()
    } else if let Some(k) = app.dk().filter(|_| !app.detail_focus) {
        let back = if app.ctx.is_some() { "Esc back  " } else { "" };
        match k {
            PK::Files if app.ctx.is_none() => format!(
                "j/k file  l/Enter diff  [ ] detail tab  v viewed  {zoom} zoom  {actions} actions"
            ),
            PK::Files => format!(
                "j/k file  l/Enter diff  v viewed  {zoom} zoom  {back}[ ] panel  {actions} actions"
            ),
            _ => format!("j/k move  l/Enter focus  {back}[ ] panel  {actions} actions"),
        }
    } else if app.detail_focus {
        match app.cur_tab() {
            Tab::Checks => {
                format!("j/k move  Enter failed-log  {actions} actions  [ ] tab  h back")
            }
            Tab::Diff => {
                format!("j/k line  n/p file  t mode  w wrap  v viewed  {zoom} zoom  h back")
            }
            _ => format!(
                "j/k scroll  [ ] tab  {actions} actions  h back  {} open  {} copy",
                kl(Act::Open),
                kl(Act::CopyUrl)
            ),
        }
    } else if app.ctx.is_some() {
        "j/k move".into()
    } else {
        format!(
            "j/k move  Enter drill in  [ ] detail tab  {{ }} list tab  {filter} filter  {actions} actions  {} repos",
            kl(Act::Browser)
        )
    };
    format!("{ctx}  {} help  {} quit{flt}", kl(Act::Help), kl(Act::Quit))
}

fn bordered(app: &App, title: String, on: bool) -> Block<'static> {
    bordered_line(app, Line::raw(title), on)
}

fn bordered_line(app: &App, title: Line<'static>, on: bool) -> Block<'static> {
    let th = &app.theme;
    let col = if on { th.accent } else { th.muted };
    let ts = if on {
        Style::new().fg(col).bold()
    } else {
        Style::new().fg(col)
    };
    Block::bordered()
        .border_set(th.border)
        .border_style(Style::new().fg(col))
        .title(title.style(ts))
}

fn popup_block(app: &App, title: String) -> Block<'static> {
    let th = &app.theme;
    Block::bordered()
        .border_set(th.border)
        .border_style(Style::new().fg(th.warn))
        .title(Span::styled(title, Style::new().fg(th.warn).bold()))
}

fn status_icon(th: &Theme, state: &str) -> Span<'static> {
    let (s, c) = match state.to_lowercase().as_str() {
        "success" | "pass" | "active" => (th.ic.ok, th.ok),
        "failure" | "fail" | "timed_out" => (th.ic.fail, th.err),
        "in_progress" | "pending" | "waiting" => (th.ic.pending, th.warn),
        "queued" => (th.ic.queued, th.muted),
        _ => (th.ic.skip, th.muted),
    };
    Span::styled(format!("{s} "), Style::new().fg(c))
}

fn state_color(th: &Theme, state: &str) -> Color {
    match state {
        "open" | "unread" | "latest" => th.ok,
        "merged" => th.merged,
        "closed" => th.err,
        _ => th.muted,
    }
}

fn label(app: &App, it: &Item, show_repo: bool, w: usize) -> Line<'static> {
    let th = &app.theme;
    let muted = Style::new().fg(th.muted);
    let mut v = vec![];
    match it.state.as_str() {
        "unread" => v.push(Span::styled(
            format!("{} ", th.ic.unread),
            Style::new().fg(th.accent),
        )),
        "read" => v.push(Span::raw("  ")),
        _ => {}
    }
    if show_repo && !it.repo.is_empty() {
        v.push(Span::styled(format!("{} ", it.repo), muted));
    }
    match it.kind {
        Kind::Pr | Kind::Issue => {
            v.push(Span::styled(format!("#{} ", it.number), muted));
            if it.is_draft {
                v.push(Span::styled("[draft] ", muted));
            }
            v.push(Span::raw(it.title.clone()));
        }
        Kind::Run => {
            v.push(status_icon(th, &it.state));
            v.push(Span::raw(it.title.clone()));
            v.push(Span::styled(format!("  {}", it.meta), muted));
        }
        Kind::Workflow => {
            v.push(status_icon(th, &it.state));
            v.push(Span::raw(it.title.clone()));
        }
        Kind::Branch => {
            v.push(Span::styled(th.ic.branch, muted));
            v.push(Span::raw(it.title.clone()));
            if it.number > 0 {
                v.push(Span::styled(
                    format!(" #{}", it.number),
                    Style::new().fg(th.accent),
                ));
            }
        }
        Kind::Release => {
            if it.state == "latest" {
                v.push(Span::styled(
                    format!("{} ", th.ic.star),
                    Style::new().fg(th.warn),
                ));
            }
            v.push(Span::raw(it.title.clone()));
            if !it.state.is_empty() && it.state != "latest" {
                v.push(Span::styled(format!(" {}", it.state), muted));
            }
        }
        Kind::Check => {
            v.push(status_icon(th, &it.state));
            v.push(Span::raw(it.title.clone()));
            v.push(Span::styled(format!("  {}", it.meta), muted));
        }
        Kind::Comment => {
            let col = if it.state == "resolved" {
                th.muted
            } else {
                th.accent
            };
            v.push(Span::styled(it.title.clone(), Style::new().fg(col)));
        }
        Kind::File => return file_label(app, it, w),
        Kind::Other | Kind::Status => v.push(Span::raw(it.title.clone())),
    }
    Line::from(v)
}

enum Note {
    Loading,
    Err(String),
    Empty(&'static str),
}

fn note(app: &App, n: Note) -> Line<'static> {
    let th = &app.theme;
    match n {
        Note::Loading => Line::from(vec![
            Span::styled(spinner(app), Style::new().fg(th.accent)),
            Span::styled(" loading...", Style::new().fg(th.muted)),
        ]),
        Note::Err(e) => Line::from(vec![
            Span::styled(format!("{} ", th.ic.fail), Style::new().fg(th.err)),
            Span::styled(e, Style::new().fg(th.err)),
        ]),
        Note::Empty(m) => Line::styled(m, Style::new().fg(th.muted).italic()),
    }
}

fn panel(f: &mut Frame, app: &App, i: usize, area: Rect, compact: bool) {
    let th = &app.theme;
    let p = &app.panels[i];
    let focused = i == app.focus;
    let items = app.visible(i);
    let tab = p
        .tabs
        .get(p.tab)
        .map(|t| format!(" {} {t}", th.ic.dot))
        .unwrap_or_default();
    let count = match (p.kind, items.len()) {
        (PK::Status, _) => String::new(),
        (_, n) if p.items.len() >= LIMIT => format!(" ({n}+)"),
        (_, n) => format!(" ({n})"),
    };
    let title = format!(" [{}] {}{tab}{count} ", i + 1, p.title);
    if compact && !focused {
        let t = title.trim().to_string();
        return f.render_widget(
            Paragraph::new(Span::styled(t, Style::new().fg(th.muted))),
            area,
        );
    }
    let block = bordered(app, title, focused && !app.detail_focus);
    let inner = block.inner(area);

    let cur = items.get(p.cursor.min(items.len().saturating_sub(1)));
    let (Some(_), None) = (cur, &p.error) else {
        let n = match &p.error {
            Some(e) => Note::Err(e.clone()),
            None if p.loading => Note::Loading,
            None if p.kind.derived() && app.pr_item().is_none() => {
                Note::Empty("select a pull request")
            }
            None => Note::Empty(match p.kind {
                PK::Files => "no files",
                PK::Checks => "no checks",
                PK::Comments => "no comments",
                _ => "nothing here",
            }),
        };
        return f.render_widget(Paragraph::new(note(app, n)).block(block), area);
    };
    let sr = app.global || p.kind == PK::Notifs;
    let lw = inner.width as usize;
    if !focused {
        let cur = cur.map(|c| label(app, c, sr, lw)).unwrap_or_default();
        return f.render_widget(Paragraph::new(cur).block(block), area);
    }
    let list = List::new(items.iter().map(|it| ListItem::new(label(app, it, sr, lw))))
        .block(block)
        .highlight_style(Style::new().bg(th.sel_bg).bold());
    let mut st = ListState::default().with_selected(Some(p.cursor.min(items.len() - 1)));
    f.render_stateful_widget(list, area, &mut st);
    let mut h = app.hit.borrow_mut();
    h.list = inner;
    h.list_off = st.offset();
}

fn wrap(s: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut out = vec![];
    for line in s.replace('\r', "").lines() {
        let (mut cur, mut n) = (String::new(), 0);
        for word in line.split(' ') {
            let wl = word.chars().count();
            if n > 0 && n + 1 + wl > w {
                out.push(std::mem::take(&mut cur));
                n = 0;
            }
            if n > 0 {
                cur.push(' ');
                n += 1;
            }
            for ch in word.chars() {
                if n == w {
                    out.push(std::mem::take(&mut cur));
                    n = 0;
                }
                cur.push(ch);
                n += 1;
            }
        }
        out.push(cur);
    }
    out
}

/// Scroll position for `len` rows in `h` visible, keeping `sel` in view; records it for the key handlers.
fn window(app: &App, h: usize, len: usize, sel: Option<(usize, usize)>) -> usize {
    let max = len.saturating_sub(h);
    let mut top = app.scroll.get().min(max);
    if let Some((s, e)) = sel {
        if e + 1 - s > h {
            // Taller than the pane (an expanded comment): keep the view inside it. Snapping to its
            // bottom and back to its top on alternate frames made the pane flicker and tear.
            if top < s || top + h > e + 1 {
                top = s;
            }
        } else if s < top {
            top = s;
        } else if e >= top + h {
            top = e + 1 - h;
        }
    }
    top = top.min(max);
    app.scroll.set(top);
    app.view_len.set(len);
    app.view_max.set(max);
    app.view_h.set(h);
    top
}

fn pane(f: &mut Frame, app: &App, area: Rect, lines: Vec<Line>, sel: Option<(usize, usize)>) {
    let top = window(app, area.height as usize, lines.len(), sel);
    f.render_widget(Paragraph::new(lines).scroll((top as u16, 0)), area);
}

/// Right pane for a Checks (its failed-step log) or Comments (the thread) row.
fn derived_detail(f: &mut Frame, app: &App, area: Rect, k: PK) {
    let it = app.selected_in(app.focus);
    let title = match (k, it) {
        (PK::Checks, Some(i)) => format!(" Log: {} ", i.title),
        (PK::Checks, None) => " Log ".to_string(),
        _ => " Thread ".to_string(),
    };
    let block = bordered(app, title, app.detail_focus);
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.hit.borrow_mut().body = inner;
    let Some(it) = it else {
        return f.render_widget(
            Paragraph::new(note(app, Note::Empty("nothing selected"))),
            inner,
        );
    };
    if k == PK::Checks {
        match app.data_of(it, Tab::Logs) {
            Some(Load::Done(Ok(Data::Text(t)))) => {
                let lines = t.iter().map(|s| Line::raw(s.clone())).collect();
                pane(f, app, inner, lines, None)
            }
            Some(Load::Done(Err(e))) => {
                f.render_widget(Paragraph::new(note(app, Note::Err(e.clone()))), inner)
            }
            _ => f.render_widget(Paragraph::new(note(app, Note::Loading)), inner),
        }
    } else {
        let cur = app.panels[app.focus].cursor;
        let lines = match app.data() {
            Some(Data::Comments(e)) if cur < e.len() => {
                card_lines(app, &e[cur], cur, inner.width as usize, false)
            }
            _ => vec![Line::raw(it.body.clone())],
        };
        pane(f, app, inner, lines, None)
    }
}

fn detail(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    app.hit.borrow_mut().detail = area;
    if let Some(l) = &app.log {
        let block = bordered(
            app,
            format!(" Log: {} (Esc closes) ", l.title),
            app.detail_focus,
        );
        let inner = block.inner(area);
        f.render_widget(block, area);
        app.hit.borrow_mut().body = inner;
        let lines = l.lines.iter().map(|s| Line::raw(s.as_str())).collect();
        return pane(f, app, inner, lines, None);
    }
    if let Some(k @ (PK::Checks | PK::Comments)) = app.dk() {
        return derived_detail(f, app, area, k);
    }
    let Some(it) = app.selected() else {
        return f.render_widget(bordered(app, " Detail ".into(), false), area);
    };
    let sep = format!(" {} ", th.border.vertical_left);
    let mut title = vec![Span::raw(" ")];
    let (mut x, mut tabs) = (area.x + 2, vec![]);
    for (i, t) in app.tabs().iter().enumerate() {
        let st = if i == app.dtab() {
            Style::new().fg(th.accent).bold().underlined()
        } else {
            Style::new().fg(th.muted)
        };
        let w = t.name().chars().count() as u16;
        tabs.push((x, x + w));
        x += w + sep.chars().count() as u16;
        title.push(Span::styled(t.name(), st));
        title.push(Span::styled(sep.clone(), Style::new().fg(th.muted)));
    }
    title.pop();
    title.push(Span::raw(" "));
    let block = bordered_line(app, Line::from(title), app.detail_focus);
    let inner = block.inner(area);
    f.render_widget(block, area);
    {
        let mut h = app.hit.borrow_mut();
        (h.tab_y, h.tabs, h.body) = (area.y, tabs, inner);
    }

    let tab = app.cur_tab();
    let w = inner.width as usize;
    // Item-only overviews render instantly; everything else waits on its fetch.
    if tab != Tab::Overview || matches!(it.kind, Kind::Status | Kind::Release) {
        let n = match app.load() {
            Some(Load::Done(Err(e))) => Some(Note::Err(e.clone())),
            Some(Load::Done(Ok(_))) => None,
            _ => Some(Note::Loading),
        };
        if let Some(n) = n {
            return f.render_widget(
                Paragraph::new(note(app, n)).wrap(ratatui::widgets::Wrap { trim: false }),
                inner,
            );
        }
    }
    let sel_style = Style::new().bg(th.sel_bg);
    match (tab, app.data()) {
        (Tab::Overview, d) => {
            let lines = overview(app, it, d, matches!(app.load(), Some(Load::Loading)), w);
            pane(f, app, inner, lines, None)
        }
        (Tab::Checks, Some(Data::Checks(c))) => {
            let mut lines: Vec<Line> = c
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let wf = c
                        .workflow
                        .as_deref()
                        .filter(|w| !w.is_empty())
                        .map(|w| format!("{w} / "))
                        .unwrap_or_default();
                    let dur = gh::duration(c).unwrap_or_default();
                    let l = Line::from(vec![
                        status_icon(th, &c.bucket),
                        Span::raw(format!("{wf}{}", c.name)),
                        Span::styled(format!("  {dur}"), Style::new().fg(th.muted)),
                    ]);
                    if i == app.row { l.style(sel_style) } else { l }
                })
                .collect();
            if lines.is_empty() {
                lines.push(note(app, Note::Empty("nothing here")));
            }
            pane(f, app, inner, lines, Some((app.row, app.row)))
        }
        (Tab::Comments, Some(Data::Comments(e))) => {
            let (mut lines, mut sel, mut starts) = (vec![], (0, 0), vec![]);
            for (i, en) in e.iter().enumerate() {
                let start = lines.len();
                starts.push(start);
                let on = i == app.row;
                lines.extend(card_lines(app, en, i, w, on));
                lines.push(Line::raw(""));
                if on {
                    sel = (start, lines.len() - 1);
                }
            }
            if lines.is_empty() {
                lines.push(note(app, Note::Empty("no comments")));
            }
            *app.row_starts.borrow_mut() = starts;
            pane(f, app, inner, lines, Some(sel))
        }
        (Tab::Diff, Some(Data::Files(fd))) => diff_view(f, app, fd, inner),
        (t, Some(Data::Text(txt))) => {
            let lines = txt
                .iter()
                .map(|s| text_line(th, s, t == Tab::Jobs))
                .collect();
            pane(f, app, inner, lines, None)
        }
        _ => {}
    }
}

fn wrap_spans(spans: Vec<Span<'static>>, w: usize) -> Vec<Vec<Span<'static>>> {
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    crate::diff::wrap_ranges(&text, w.max(1))
        .into_iter()
        .map(|(a, b)| slice_spans(&spans, a, b))
        .collect()
}

fn reaction_label(th: &Theme, content: &str) -> &'static str {
    let (emoji, plain) = match content {
        "THUMBS_UP" => ("\u{1f44d}", "+1"),
        "THUMBS_DOWN" => ("\u{1f44e}", "-1"),
        "LAUGH" => ("\u{1f604}", "laugh"),
        "HOORAY" => ("\u{1f389}", "hooray"),
        "CONFUSED" => ("\u{1f615}", "confused"),
        "HEART" => ("\u{2764}", "heart"),
        "ROCKET" => ("\u{1f680}", "rocket"),
        "EYES" => ("\u{1f440}", "eyes"),
        _ => ("?", "?"),
    };
    if th.ascii { plain } else { emoji }
}

fn lang_ext(lang: &str) -> &str {
    match lang.to_lowercase().as_str() {
        "rust" => "rs",
        "python" | "python3" => "py",
        "javascript" | "node" => "js",
        "typescript" => "ts",
        "bash" | "shell" | "zsh" | "console" => "sh",
        "yaml" => "yml",
        "markdown" => "md",
        "ruby" => "rb",
        "golang" => "go",
        "c++" => "cpp",
        _ => lang,
    }
}

/// Code-fence highlighter for comments (a no-op without the `syntax` feature).
type Segs = Vec<Vec<(Color, String)>>;

fn md_highlighter<'a>(app: &'a App) -> impl Fn(&str, &[String]) -> Option<Segs> + 'a {
    move |lang, code| {
        use std::hash::{Hash, Hasher};
        if lang.is_empty() {
            return None;
        }
        let th = &app.theme;
        let mut g = app.hl.borrow_mut();
        let hl = g.get_or_insert_with(|| Hl::new(th.syntect_theme(), th.rgb_fn()));
        let lines: Vec<DLine> = code
            .iter()
            .map(|t| DLine {
                op: Op::Ctx,
                old: None,
                new: None,
                text: t.clone(),
            })
            .collect();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        code.hash(&mut h);
        let key = format!("md|{lang}|{:x}", h.finish());
        hl.ensure(&key, &format!("x.{}", lang_ext(lang)), &lines, lines.len());
        (0..lines.len()).map(|i| hl.get(&key, i).cloned()).collect()
    }
}

fn role_label(role: &str) -> Option<&'static str> {
    Some(match role {
        "OWNER" => "owner",
        "MEMBER" => "member",
        "COLLABORATOR" => "collaborator",
        "CONTRIBUTOR" => "contributor",
        "FIRST_TIME_CONTRIBUTOR" | "FIRST_TIMER" => "first-timer",
        _ => return None,
    })
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `author [role] . 3d ago . edited` plus review/thread badges.
/// `badge` false leaves the review-state / `path:line` badge out (thread paths get their own line).
fn card_header(app: &App, c: &crate::gh::Card, nested: bool, badge: bool) -> Vec<Span<'static>> {
    let th = &app.theme;
    let muted = Style::new().fg(th.muted);
    let dot = Span::styled(format!(" {} ", th.ic.dot), muted);
    let mut v = vec![];
    if nested {
        v.push(Span::styled(
            if th.ascii { "> " } else { "\u{21b3} " },
            muted,
        ));
    }
    v.push(Span::styled(
        c.author.clone(),
        Style::new().fg(th.accent).bold(),
    ));
    if let Some(r) = role_label(&c.role) {
        v.push(Span::styled(format!(" [{r}]"), muted));
    }
    if badge && !c.badge.is_empty() {
        let col = match c.badge.as_str() {
            "APPROVED" => th.ok,
            "CHANGES_REQUESTED" => th.err,
            _ => th.warn,
        };
        v.push(Span::styled(format!(" {}", c.badge), Style::new().fg(col)));
    }
    let ago = crate::browse::ago(&c.when, now_secs());
    if !ago.is_empty() {
        v.push(dot.clone());
        v.push(Span::styled(format!("{ago} ago"), muted));
    }
    if c.edited {
        v.push(dot);
        v.push(Span::styled("edited", muted.italic()));
    }
    for (on, label, col) in [
        (c.resolved, "resolved", th.ok),
        (c.outdated, "outdated", th.muted),
    ] {
        if on {
            v.push(Span::styled(format!("  [{label}]"), Style::new().fg(col)));
        }
    }
    v
}

fn reactions_line(app: &App, c: &crate::gh::Card, open: bool) -> Option<Line<'static>> {
    let th = &app.theme;
    if c.reactions.is_empty() {
        return None;
    }
    let parts: Vec<String> = c
        .reactions
        .iter()
        .map(|r| {
            let l = reaction_label(th, &r.content);
            if open {
                let more = (r.count as usize).saturating_sub(r.users.len());
                let extra = if more > 0 {
                    format!(" +{more}")
                } else {
                    String::new()
                };
                format!("{l} {}{extra}", r.users.join(", "))
            } else {
                format!("{l} {}", r.count)
            }
        })
        .collect();
    Some(Line::styled(parts.join("  "), Style::new().fg(th.muted)))
}

/// One comment as a card: header, markdown body, reactions, nested replies. `idx` keys expand state.
fn card_lines(
    app: &App,
    e: &crate::gh::Entry,
    idx: usize,
    w: usize,
    selected: bool,
) -> Vec<Line<'static>> {
    let th = &app.theme;
    let key = (app.selected().map(Item::key).unwrap_or_default(), idx);
    let (expanded, open) = (app.expanded.contains(&key), app.react_open.contains(&key));
    let hl = md_highlighter(app);
    let rail = |on: bool| {
        Span::styled(
            if on { "\u{258e}" } else { " " },
            Style::new().fg(th.accent),
        )
    };
    let body = |text: &str, width: usize| {
        crate::md::render(
            text,
            th,
            &crate::md::Opts {
                width,
                expanded,
                hl: Some(&hl),
            },
        )
    };
    let mut out: Vec<Line<'static>> = vec![];
    let mut push = |indent: usize, spans: Vec<Span<'static>>| {
        let mut v = vec![rail(selected), Span::raw(" ".repeat(indent))];
        v.extend(spans);
        out.push(Line::from(v));
    };
    let c = &e.card;
    let narrow = w.saturating_sub(3).max(1);
    // Long headers wrap rather than clip, so the time and badges never fall off the edge.
    for row in wrap_spans(card_header(app, c, false, e.thread.is_none()), narrow) {
        push(0, row);
    }
    if e.thread.is_some() && !c.badge.is_empty() {
        let path = mid_ellipsis(&c.badge, narrow.saturating_sub(1), th.ic.ell);
        push(1, vec![Span::styled(path, Style::new().fg(th.warn))]);
    }
    let r = body(&c.body, w.saturating_sub(3));
    let folded = r.folded;
    for l in r.lines {
        push(1, l.spans);
    }
    if let Some(l) = reactions_line(app, c, open) {
        push(1, l.spans);
    }
    for reply in &c.replies {
        for row in wrap_spans(
            card_header(app, reply, true, true),
            w.saturating_sub(7).max(1),
        ) {
            push(3, row);
        }
        let rr = body(&reply.body, w.saturating_sub(7));
        for l in rr.lines {
            push(5, l.spans);
        }
        if let Some(l) = reactions_line(app, reply, open) {
            push(5, l.spans);
        }
    }
    if folded && !expanded {
        push(
            1,
            vec![Span::styled(
                "Enter expands",
                Style::new().fg(th.muted).italic(),
            )],
        );
    }
    if selected {
        for l in &mut out {
            *l = std::mem::take(l).style(Style::new().bg(th.sel_bg));
        }
    }
    out
}

/// Job lines come with unicode status marks; swap in the active icon set and color them.
fn text_line(th: &Theme, s: &str, jobs: bool) -> Line<'static> {
    if jobs {
        let t = s.trim_start();
        let pad = &s[..s.len() - t.len()];
        let mark = match t.chars().next() {
            Some('✓') => Some((th.ic.ok, th.ok)),
            Some('✗') => Some((th.ic.fail, th.err)),
            Some('●') => Some((th.ic.pending, th.warn)),
            Some('-') => Some((th.ic.skip, th.muted)),
            _ => None,
        };
        if let Some((m, c)) = mark {
            let rest = t.chars().skip(1).collect::<String>();
            return Line::from(vec![
                Span::raw(pad.to_string()),
                Span::styled(m, Style::new().fg(c)),
                Span::raw(rest),
            ]);
        }
    }
    Line::raw(s.to_string())
}

fn overview(
    app: &App,
    it: &Item,
    data: Option<&Data>,
    loading: bool,
    w: usize,
) -> Vec<Line<'static>> {
    let th = &app.theme;
    let muted = Style::new().fg(th.muted);
    if let Some(Data::Text(t)) = data {
        return t.iter().flat_map(|s| wrap(s, w)).map(Line::raw).collect();
    }
    let ov = match data {
        Some(Data::Overview(o)) => Some(o),
        _ => None,
    };
    let state = if ov.is_some_and(|o| o.merged_at.is_some()) {
        "merged".to_string()
    } else if it.is_draft || ov.is_some_and(|o| o.is_draft) {
        "draft".into()
    } else {
        it.state.to_lowercase()
    };
    let mut v = vec![Line::styled(it.title.clone(), Style::new().bold())];
    let id = if it.number > 0 {
        format!(" #{}", it.number)
    } else {
        String::new()
    };
    v.push(Line::from(vec![
        Span::styled(format!("{}{id}  ", it.repo), muted),
        Span::styled(
            format!("[{state}]"),
            Style::new().fg(state_color(th, &state)),
        ),
    ]));
    v.push(Line::styled(th.border.horizontal_top.repeat(w), muted));
    let kv = |k: &str, val: String| {
        Line::from(vec![
            Span::styled(format!("{k:<10}"), muted),
            Span::raw(val),
        ])
    };
    if !it.author.login.is_empty() {
        v.push(kv("author", it.author.login.clone()));
    }
    if !it.meta.is_empty() {
        v.push(Line::styled(it.meta.clone(), muted));
    }
    if let Some(o) = ov {
        v.push(kv(
            "branch",
            format!("{} <- {}", o.base_ref_name, o.head_ref_name),
        ));
        v.push(kv("mergeable", o.mergeable.to_lowercase()));
        if let Some(d) = o.review_decision.as_deref().filter(|d| !d.is_empty()) {
            v.push(kv("decision", d.to_string()));
        }
        for r in &o.latest_reviews {
            let c = match r.state.as_str() {
                "APPROVED" => th.ok,
                "CHANGES_REQUESTED" => th.err,
                _ => th.muted,
            };
            v.push(Line::from(vec![
                Span::raw(format!(
                    "  {} ",
                    r.author.as_ref().map_or("ghost", |a| &a.login)
                )),
                Span::styled(r.state.clone(), Style::new().fg(c)),
            ]));
        }
        for r in &o.review_requests {
            let n = r.login.as_deref().or(r.name.as_deref()).unwrap_or("?");
            v.push(Line::from(vec![
                Span::raw(format!("  {n} ")),
                Span::styled("(requested)", muted),
            ]));
        }
    } else if loading {
        v.push(Line::from(vec![
            Span::styled(spinner(app), Style::new().fg(th.accent)),
            Span::styled(" loading details...", muted),
        ]));
    }
    if !it.labels.is_empty() {
        let names = it
            .labels
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        v.push(kv("labels", names));
    }
    v.push(Line::raw(""));
    let hl = md_highlighter(app);
    let body = crate::md::render(
        &it.body,
        th,
        &crate::md::Opts {
            width: w,
            expanded: true,
            hl: Some(&hl),
        },
    );
    v.extend(body.lines);
    v
}

fn numw(n: Option<u32>, nw: usize) -> String {
    n.map_or(" ".repeat(nw), |n| format!("{n:>nw$}"))
}

/// Char ranges (prefix/suffix trimmed) that differ between a removed and an added line.
fn changed(a: &str, b: &str) -> Option<((usize, usize), (usize, usize))> {
    let (ca, cb): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let p = ca.iter().zip(&cb).take_while(|(x, y)| x == y).count();
    let s = ca[p..]
        .iter()
        .rev()
        .zip(cb[p..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    (p + s > 0).then_some(((p, ca.len() - s), (p, cb.len() - s)))
}

fn slice_chars(s: &str, a: usize, b: usize) -> String {
    s.chars().skip(a).take(b - a).collect()
}

/// Shortens `s` to `w` cells keeping both ends ("src/lo…ng.rs"); the start is never cut off.
pub fn mid_ellipsis(s: &str, w: usize, ell: &str) -> String {
    use unicode_width::UnicodeWidthStr;
    if s.width() <= w {
        return s.to_string();
    }
    let ew = ell.width();
    if w <= ew {
        return ell.chars().take(w).collect();
    }
    let keep = w - ew;
    let (lw, rw) = (keep - keep / 2, keep / 2);
    let take = |it: &mut dyn Iterator<Item = char>, budget: usize| {
        let (mut out, mut used) = (vec![], 0);
        for c in it {
            let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if used + cw > budget {
                break;
            }
            used += cw;
            out.push(c);
        }
        out
    };
    let left: String = take(&mut s.chars(), lw).into_iter().collect();
    let mut right = take(&mut s.chars().rev(), rw);
    right.reverse();
    format!("{left}{ell}{}", right.into_iter().collect::<String>())
}

struct Paint<'a> {
    th: &'a Theme,
    hl: &'a Hl,
    key: &'a str,
}

impl Paint<'_> {
    /// Syntax-colored spans for line `idx` (plain +/- colors without highlighting), with an
    /// optional stronger background over a changed char range.
    fn text(
        &self,
        idx: usize,
        d: &DLine,
        bg: Option<Color>,
        strong: Option<((usize, usize), Color)>,
    ) -> Vec<Span<'static>> {
        let th = self.th;
        let fallback = match d.op {
            Op::Add => th.ok,
            Op::Del => th.err,
            _ => th.text,
        };
        let src = match self.hl.get(self.key, idx) {
            Some(s) if !s.is_empty() => s.clone(),
            _ => vec![(fallback, d.text.clone())],
        };
        let (mut out, mut n) = (vec![], 0usize);
        for (c, s) in src {
            let (mut run, mut run_strong) = (String::new(), false);
            let flush = |run: &mut String, strong_on: bool, out: &mut Vec<Span<'static>>| {
                if run.is_empty() {
                    return;
                }
                let b = if strong_on { strong.map(|s| s.1) } else { bg };
                out.push(Span::styled(
                    std::mem::take(run),
                    Style::new().fg(c).patch_bg(b),
                ));
            };
            for ch in s.chars() {
                let st = strong.is_some_and(|((a, b), _)| n >= a && n < b);
                if st != run_strong {
                    flush(&mut run, run_strong, &mut out);
                    run_strong = st;
                }
                run.push(ch);
                n += 1;
            }
            flush(&mut run, run_strong, &mut out);
        }
        out
    }

    fn bg(&self, op: Op, selected: bool) -> Option<Color> {
        if selected {
            return Some(self.th.sel_bg);
        }
        match op {
            Op::Add => Some(self.th.add_bg),
            Op::Del => Some(self.th.del_bg),
            _ => None,
        }
    }

    /// Stronger background over the words that changed between this line and its pair.
    fn strong(&self, file: &diff::File, i: usize) -> Option<((usize, usize), Color)> {
        let d = &file.lines[i];
        let o = file.partner.get(i).copied().flatten()?;
        let (del, add) = if d.op == Op::Del {
            (d, &file.lines[o])
        } else {
            (&file.lines[o], d)
        };
        let (l, r) = changed(&del.text, &add.text)?;
        Some(if d.op == Op::Del {
            (l, self.th.del_bg2)
        } else {
            (r, self.th.add_bg2)
        })
    }

    fn rows(&self, text: &str, tw: usize, wrap: bool) -> Vec<(usize, usize)> {
        let mut r = diff::wrap_ranges(text, tw);
        if !wrap {
            r.truncate(1);
        }
        r
    }

    /// Full-width hunk header bar.
    fn hunk(&self, text: &str, w: usize, selected: bool, wrap: bool) -> Vec<Line<'static>> {
        let th = self.th;
        let bg = if selected { th.sel_bg } else { th.hunk_bg };
        self.rows(text, w, wrap)
            .into_iter()
            .map(|(a, b)| {
                let s = slice_chars(text, a, b);
                let pad = w.saturating_sub(Span::raw(s.clone()).width());
                Line::from(vec![
                    Span::styled(s, Style::new().fg(th.hunk).bg(bg).bold()),
                    Span::styled(" ".repeat(pad), Style::new().bg(bg)),
                ])
            })
            .collect()
    }

    /// One logical unified line as 1+ display rows (blank gutter and a continuation marker after the first).
    fn unified(&self, file: &diff::File, i: usize, selected: bool, g: &Geo) -> Vec<Line<'static>> {
        let (th, d) = (self.th, &file.lines[i]);
        if d.op == Op::Hunk {
            return self.hunk(&d.text, g.w, selected, g.wrap);
        }
        let bg = self.bg(d.op, selected);
        let spans = self.text(i, d, bg, if selected { None } else { self.strong(file, i) });
        let (sign, sc) = match d.op {
            Op::Add => ("+", th.ok),
            Op::Del => ("-", th.err),
            _ => (" ", th.muted),
        };
        let gut = format!("{} {} ", numw(d.old, g.nw), numw(d.new, g.nw));
        self.rows(&d.text, g.tw_u, g.wrap)
            .into_iter()
            .enumerate()
            .map(|(k, (a, b))| {
                let (gutter, mark) = if k == 0 {
                    (
                        gut.clone(),
                        Span::styled(sign, Style::new().fg(sc).patch_bg(bg)),
                    )
                } else {
                    (
                        " ".repeat(gut.chars().count()),
                        Span::styled(th.ic.cont, Style::new().fg(th.muted).patch_bg(bg)),
                    )
                };
                let mut v = vec![
                    Span::styled(gutter, Style::new().fg(th.muted).patch_bg(bg)),
                    mark,
                ];
                v.extend(slice_spans(&spans, a, b));
                let used = Line::from(v.clone()).width();
                if bg.is_some() {
                    v.push(Span::styled(
                        " ".repeat(g.w.saturating_sub(used)),
                        Style::new().patch_bg(bg),
                    ));
                }
                Line::from(v)
            })
            .collect()
    }

    /// One side of a split row; every returned row is exactly `g.half` cells wide.
    fn side(
        &self,
        file: &diff::File,
        idx: Option<usize>,
        left: bool,
        selected: bool,
        g: &Geo,
    ) -> Vec<Vec<Span<'static>>> {
        let th = self.th;
        let Some(i) = idx else {
            let st = Style::new().patch_bg(selected.then_some(th.sel_bg));
            return vec![vec![Span::styled(" ".repeat(g.half), st)]];
        };
        let d = &file.lines[i];
        let bg = self.bg(d.op, selected);
        let spans = self.text(i, d, bg, if selected { None } else { self.strong(file, i) });
        let num = numw(if left { d.old } else { d.new }, g.nw);
        self.rows(&d.text, g.tw_s, g.wrap)
            .into_iter()
            .enumerate()
            .map(|(k, (a, b))| {
                let gutter = if k == 0 {
                    format!("{num} ")
                } else {
                    format!("{}{}", " ".repeat(g.nw), th.ic.cont)
                };
                let mut v = vec![Span::styled(gutter, Style::new().fg(th.muted).patch_bg(bg))];
                v.extend(slice_spans(&spans, a, b));
                let used = Line::from(v.clone()).width();
                v.push(Span::styled(
                    " ".repeat(g.half.saturating_sub(used)),
                    Style::new().patch_bg(bg),
                ));
                v
            })
            .collect()
    }

    fn split_row(
        &self,
        file: &diff::File,
        a: Option<usize>,
        b: Option<usize>,
        selected: bool,
        g: &Geo,
    ) -> Vec<Line<'static>> {
        let (l, r) = (
            self.side(file, a, true, selected, g),
            self.side(file, b, false, selected, g),
        );
        let blank = |sel: bool| {
            vec![Span::styled(
                " ".repeat(g.half),
                Style::new().patch_bg(sel.then_some(self.th.sel_bg)),
            )]
        };
        let mid = Span::styled(self.th.border.vertical_left, Style::new().fg(self.th.muted));
        (0..l.len().max(r.len()))
            .map(|k| {
                let mut v = l.get(k).cloned().unwrap_or_else(|| blank(selected));
                v.push(mid.clone());
                v.extend(r.get(k).cloned().unwrap_or_else(|| blank(selected)));
                Line::from(v)
            })
            .collect()
    }
}

trait PatchBg {
    fn patch_bg(self, bg: Option<Color>) -> Self;
}

impl PatchBg for Style {
    fn patch_bg(self, bg: Option<Color>) -> Self {
        bg.map_or(self, |b| self.bg(b))
    }
}

/// Widths for one diff render.
struct Geo {
    w: usize,
    nw: usize,
    tw_u: usize,
    half: usize,
    tw_s: usize,
    wrap: bool,
}

fn text_h(s: &str, tw: usize, wrap: bool) -> usize {
    use unicode_width::UnicodeWidthStr;
    if !wrap || s.width() <= tw {
        1
    } else {
        diff::wrap_ranges(s, tw).len()
    }
}

fn diff_view(f: &mut Frame, app: &App, fd: &FilesData, area: Rect) {
    let th = &app.theme;
    let files = &fd.files;
    let fi = app.file().min(files.len().saturating_sub(1));
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    app.hit.borrow_mut().body = body;
    let Some(file) = files.get(fi) else {
        return f.render_widget(Paragraph::new(note(app, Note::Empty("no changes"))), area);
    };
    let split = diff::use_split(app.mode, body.width as usize);
    app.eff_split.set(split);

    // Sticky header: which file, how big, and the current view mode.
    let mode = match (app.mode, split) {
        (DiffMode::Auto, s) => format!("auto:{}", if s { "split" } else { "unified" }),
        (DiffMode::Unified, _) => "unified".into(),
        (DiffMode::Split, _) => "split".into(),
    };
    let viewed = app.viewed.contains(&(app.pr_key(), file.path.clone()));
    let mut hdr = vec![
        Span::styled(file.path.clone(), Style::new().bold()),
        Span::styled(
            format!("  {}/{}  ", fi + 1, files.len()),
            Style::new().fg(th.muted),
        ),
        Span::styled(format!("+{}", file.adds), Style::new().fg(th.ok)),
        Span::raw(" "),
        Span::styled(format!("-{}", file.dels), Style::new().fg(th.err)),
    ];
    if viewed {
        hdr.push(Span::styled(
            format!("  {} viewed", th.ic.viewed),
            Style::new().fg(th.ok),
        ));
    }
    hdr.push(Span::styled(
        format!(
            "  [{mode}, {}]  t mode  w wrap  v viewed",
            if app.wrap { "wrap" } else { "clip" }
        ),
        Style::new().fg(th.muted),
    ));
    f.render_widget(
        Paragraph::new(Line::from(hdr)).style(Style::new().bg(th.sel_bg)),
        head,
    );

    // Geometry. Line numbers sized to the file; text widths exclude gutters.
    let w = body.width as usize;
    let maxn = file
        .lines
        .iter()
        .filter_map(|l| l.old.max(l.new))
        .max()
        .unwrap_or(0);
    let nw = maxn.to_string().len().max(3);
    let half = w.saturating_sub(1) / 2;
    let g = Geo {
        w,
        nw,
        tw_u: w.saturating_sub(2 * nw + 3).max(1),
        half,
        tw_s: half.saturating_sub(nw + 1).max(1),
        wrap: app.wrap,
    };

    // Display height of every logical row (soft-wrap makes them differ), then the visible window.
    let rows = split.then(|| diff::split(&file.lines));
    let heights: Vec<usize> = match &rows {
        None => file
            .lines
            .iter()
            .map(|d| text_h(&d.text, if d.op == Op::Hunk { g.w } else { g.tw_u }, g.wrap))
            .collect(),
        Some(rows) => rows
            .iter()
            .map(|r| match r {
                Row::Full(i) => text_h(&file.lines[*i].text, g.w, g.wrap),
                Row::Pair(a, b) => [a, b]
                    .iter()
                    .filter_map(|x| x.map(|i| text_h(&file.lines[i].text, g.tw_s, g.wrap)))
                    .max()
                    .unwrap_or(1),
            })
            .collect(),
    };
    let mut starts = Vec::with_capacity(heights.len());
    let mut total = 0;
    for h in &heights {
        starts.push(total);
        total += h;
    }
    let cur = app.row.min(heights.len().saturating_sub(1));
    let sel = starts.get(cur).map(|s| (*s, s + heights[cur] - 1));
    let h = body.height as usize;
    let top = window(app, h, total, sel);
    let r0 = starts.partition_point(|&s| s <= top).saturating_sub(1);
    let mut r1 = r0;
    while r1 + 1 < starts.len() && starts[r1 + 1] < top + h {
        r1 += 1;
    }
    let upto = match &rows {
        None => r1 + 1,
        Some(r) => r[r0..=r1.min(r.len().saturating_sub(1))]
            .iter()
            .map(|row| match row {
                Row::Full(i) => *i,
                Row::Pair(a, b) => a.iter().chain(b.iter()).copied().max().unwrap_or(0),
            })
            .max()
            .map_or(0, |m| m + 1),
    };
    *app.row_starts.borrow_mut() = starts.clone();

    let key = format!("{}|{}", app.pr_key(), file.path);
    let mut guard = app.hl.borrow_mut();
    let hl = guard.get_or_insert_with(|| Hl::new(th.syntect_theme(), th.rgb_fn()));
    hl.ensure(&key, &file.path, &file.lines, upto);
    let paint = Paint { th, hl, key: &key };

    let mut lines: Vec<Line> = vec![];
    for r in r0..=r1.min(heights.len().saturating_sub(1)) {
        if heights.is_empty() {
            break;
        }
        let rl = match &rows {
            None => paint.unified(file, r, r == cur, &g),
            Some(rows) => match &rows[r] {
                Row::Full(i) => paint.unified(file, *i, r == cur, &g),
                Row::Pair(a, b) => paint.split_row(file, *a, *b, r == cur, &g),
            },
        };
        let skip = if r == r0 { top - starts[r0] } else { 0 };
        lines.extend(rl.into_iter().skip(skip));
    }
    lines.truncate(h);
    f.render_widget(Paragraph::new(lines), body);
}

fn popup(f: &mut Frame, app: &App, w: u16, h: u16, title: &str) -> Rect {
    let area = f
        .area()
        .centered(Constraint::Length(w), Constraint::Length(h));
    f.render_widget(Clear, area);
    let block = popup_block(app, format!(" {title} "));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

fn modal(f: &mut Frame, app: &App) {
    let Some(m) = &app.modal else { return };
    let th = &app.theme;
    let bar = Style::new().bg(th.sel_bg).bold();
    match m {
        Modal::Menu(items, i) => {
            let area = popup(
                f,
                app,
                60,
                items.len() as u16 + 2,
                "Actions (Enter pick, Esc cancel)",
            );
            let list = List::new(items.iter().map(|a| a.label.as_str())).highlight_style(bar);
            f.render_stateful_widget(
                list,
                area,
                &mut ListState::default().with_selected(Some(*i)),
            );
        }
        Modal::Input { title, buf, .. } => {
            let area = popup(
                f,
                app,
                74,
                12,
                &format!("{title}  [Ctrl-S submit, Esc cancel]"),
            );
            let w = area.width as usize;
            let lines: Vec<Line> = wrap(&format!("{buf}_"), w)
                .into_iter()
                .map(Line::raw)
                .collect();
            let skip = lines.len().saturating_sub(area.height as usize);
            f.render_widget(Paragraph::new(lines).scroll((skip as u16, 0)), area);
        }
        Modal::Confirm(c) => {
            // Always the full command: grow to fit, scroll (j/k) only if the terminal is too small.
            let w = f.area().width.saturating_sub(4).clamp(20, 100);
            let lines = wrap(&gh::shell(&c.cmd), w as usize - 2);
            let h = (lines.len() as u16 + 2).min(f.area().height.saturating_sub(2));
            let hint = if lines.len() as u16 + 2 > h {
                "j/k scroll, "
            } else {
                ""
            };
            let area = popup(
                f,
                app,
                w,
                h,
                &format!("Run this command?  [{hint}y yes, n/Esc no]"),
            );
            c.max_scroll
                .set((lines.len() as u16).saturating_sub(area.height));
            let lines: Vec<Line> = lines.into_iter().map(Line::raw).collect();
            f.render_widget(Paragraph::new(lines).scroll((c.scroll, 0)), area);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gh::{Check, Entry};
    use crate::theme::IconSet;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    fn app(light: bool, icons: IconSet) -> App {
        App::with("o/r".into(), Theme::new(light, icons, true), false)
    }

    fn render_app(app: &App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let b = t.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| b[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render(w: u16, h: u16, light: bool, icons: IconSet) -> String {
        render_app(&app(light, icons), w, h)
    }

    fn key(app: &mut App, c: KeyCode) {
        app.on_key(KeyEvent::new(c, KeyModifiers::NONE));
    }

    const LONG: &str = "This is a deliberately long prose line in a markdown file that has to wrap in a narrow pane instead of being clipped away";

    fn seeded() -> App {
        let mut a = app(false, IconSet::Unicode);
        let pr = Item {
            number: 7,
            title: "Add notes".into(),
            repo: "o/r".into(),
            state: "open".into(),
            kind: Kind::Pr,
            ..Default::default()
        };
        let diff = format!(
            "diff --git a/docs/notes.md b/docs/notes.md\n--- a/docs/notes.md\n+++ b/docs/notes.md\n@@ -1,2 +1,2 @@\n keep\n-old line\n+{LONG}\n\
diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-a\n+b\n"
        );
        let files = crate::diff::parse(&diff);
        let threads = [("src/main.rs".to_string(), 2u32)].into();
        let checks = vec![Check {
            name: "build".into(),
            bucket: "pass".into(),
            ..Default::default()
        }];
        let card = crate::gh::Card {
            author: "alice".into(),
            role: "MEMBER".into(),
            when: "2026-01-02T00:00:00Z".into(),
            edited: true,
            badge: "src/main.rs:1".into(),
            body: "please **rename** this\n\n- first\n- second\n\n```rust\nlet x = 1;\n```\n"
                .into(),
            reactions: vec![crate::gh::Reaction {
                content: "THUMBS_UP".into(),
                count: 2,
                users: vec!["hubot".into(), "monalisa".into()],
            }],
            replies: vec![crate::gh::Card {
                author: "octocat".into(),
                body: "done, see [diff](https://example.com/d)".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let comments = vec![Entry {
            head: "thread src/main.rs:1".into(),
            body: "please rename".into(),
            resolved: false,
            thread: None,
            card,
        }];
        a.seed(
            pr,
            vec![
                (
                    Tab::Diff,
                    Data::Files(FilesData {
                        sha: "abc".into(),
                        files,
                        threads,
                    }),
                ),
                (Tab::Checks, Data::Checks(checks)),
                (Tab::Comments, Data::Comments(comments)),
            ],
        );
        key(&mut a, KeyCode::Char('2'));
        a
    }

    #[test]
    fn layout_in_both_themes_and_sizes() {
        for light in [false, true] {
            for (w, h) in [(120, 40), (80, 24), (50, 12)] {
                let s = render(w, h, light, IconSet::Unicode);
                for want in [
                    "[1] Status",
                    "[2] Pull requests",
                    "[3] Files",
                    "[8] Notifications",
                    "╭",
                    "Detail",
                ] {
                    assert!(
                        s.contains(want),
                        "{w}x{h} light={light} missing {want:?}\n{s}"
                    );
                }
            }
        }
    }

    #[test]
    fn files_panel_follows_pr_and_diff_wraps() {
        for (w, h) in [(80, 24), (120, 40)] {
            let mut a = seeded();
            let s = render_app(&a, w, h);
            assert!(s.contains("[3] Files (2)"), "{w}x{h}\n{s}");
            key(&mut a, KeyCode::Char('3')); // focus Files: diff of file 1 shown, full width
            let s = render_app(&a, w, h);
            for want in [
                "M ",
                "+1 -1",
                "notes.md",
                "docs/",
                "1/2",
                "auto:unified",
                "wrap",
            ] {
                assert!(s.contains(want), "{w}x{h} missing {want:?}\n{s}");
            }
            // soft-wrap: the long prose line continues on a marked row and nothing is cut off
            assert!(s.contains('↪') && s.contains("away"), "{w}x{h}\n{s}");
            key(&mut a, KeyCode::Char('w')); // clip: no continuation rows
            let s = render_app(&a, w, h);
            assert!(!s.contains('↪') && s.contains("clip"), "{w}x{h}\n{s}");
            key(&mut a, KeyCode::Char('j')); // next file; diff follows
            assert!(render_app(&a, w, h).contains("src/main.rs"));
        }
    }

    #[test]
    fn badges_viewed_and_zoom() {
        let mut a = seeded();
        key(&mut a, KeyCode::Char('3'));
        key(&mut a, KeyCode::Char('j'));
        let s = render_app(&a, 120, 40);
        assert!(s.contains("◆2"), "thread badge\n{s}");
        key(&mut a, KeyCode::Char('v'));
        assert!(render_app(&a, 120, 40).contains('✓'));
        key(&mut a, KeyCode::Char('f'));
        let s = render_app(&a, 160, 30);
        assert!(
            !s.contains("[3] Files") && s.contains("auto:split"),
            "zoomed:\n{s}"
        );
        key(&mut a, KeyCode::Esc);
        assert!(render_app(&a, 120, 40).contains("[3] Files"));
    }

    #[test]
    fn drill_in_and_back_keeps_cursor() {
        for (w, h) in [(80, 24), (120, 40)] {
            let mut a = seeded();
            key(&mut a, KeyCode::Enter); // PR list focus: drill in
            let s = render_app(&a, w, h);
            for want in ["› PR #7", "[1] Files", "[2] Checks", "[3] Comments"] {
                assert!(s.contains(want), "{w}x{h} missing {want:?}\n{s}");
            }
            assert!(!s.contains("[2] Pull requests"));
            key(&mut a, KeyCode::Char(']')); // next panel: Checks
            assert!(render_app(&a, w, h).contains("Log: build"));
            key(&mut a, KeyCode::Char(']')); // Comments: thread text
            assert!(render_app(&a, w, h).contains("please rename"));
            key(&mut a, KeyCode::Esc);
            let s = render_app(&a, w, h);
            assert!(
                s.contains("[2] Pull requests") && !s.contains("› PR"),
                "{s}"
            );
        }
    }

    #[test]
    fn g_key_depends_on_focus() {
        let mut a = seeded(); // PR list focus
        key(&mut a, KeyCode::Char('G'));
        assert!(a.global, "list focus toggles the global view");
        key(&mut a, KeyCode::Char('G'));
        assert!(!a.global);
        key(&mut a, KeyCode::Char('3')); // Files focus: last file, no toggle, and it says why
        key(&mut a, KeyCode::Char('G'));
        assert!(
            !a.global && a.file() == 1 && a.status.contains("last row"),
            "{}",
            a.status
        );
        let mut a = seeded(); // (the global toggle above cleared the cache)
        key(&mut a, KeyCode::Enter); // drill-in: bottom of the focused list
        key(&mut a, KeyCode::Char('G'));
        assert!(a.ctx.is_some(), "in drill-in");
        assert!(!a.global, "no toggle");
        assert_eq!(a.file(), 1, "{}", a.status);
    }

    #[test]
    fn help_popup_fits_or_scrolls() {
        let mut a = seeded();
        key(&mut a, KeyCode::Char('?'));
        let s = render_app(&a, 200, 50);
        for want in [
            "(Esc returns)",
            "Mine/Review/...)",
            "m merge",
            "f zoom",
            "g/G first/last row",
        ] {
            assert!(s.contains(want), "200 cols: missing {want:?}\n{s}");
        }
        let s = render_app(&a, 80, 24); // too small for everything: wraps and scrolls
        assert!(
            s.contains("Keys (j/k scroll") && s.contains("returns)"),
            "{s}"
        );
        for _ in 0..40 {
            key(&mut a, KeyCode::Char('j'));
        }
        let s = render_app(&a, 80, 24);
        assert!(s.contains("quit"), "scrolled to the end\n{s}");
        key(&mut a, KeyCode::Char('x')); // any other key closes
        assert!(!a.help && !render_app(&a, 80, 24).contains("Keys"));
    }

    #[test]
    fn files_focus_switches_detail_tabs() {
        let mut a = seeded();
        key(&mut a, KeyCode::Char('3'));
        assert!(render_app(&a, 120, 40).contains("auto:unified")); // Diff by default
        key(&mut a, KeyCode::Char(']'));
        let s = render_app(&a, 120, 40);
        assert!(s.contains("[open]") && !s.contains("auto:unified"), "{s}");
    }

    #[test]
    fn viewed_tick_keeps_stats_aligned() {
        let mut a = seeded();
        key(&mut a, KeyCode::Char('3'));
        key(&mut a, KeyCode::Char('v')); // mark file 1 viewed
        let s = render_app(&a, 120, 40);
        let col = |name: &str| {
            // rows of the Files panel: they start with the status letter
            let line = s
                .lines()
                .find(|l| l.starts_with("│M ") && l.contains(name))
                .unwrap();
            line.chars().take(45).position(|c| c == '+').unwrap()
        };
        assert_eq!(col("notes.md"), col("main.rs"), "{s}");
    }

    #[test]
    fn repo_browser_layout() {
        let mut a = seeded();
        let page = crate::browse::parse_page(include_str!("../tests/repos.json")).unwrap();
        let mut b = Browser::new();
        (b.viewer, b.rows, b.loading) = (page.viewer, page.rows, true);
        a.browser = Some(b);
        a.cfg.repos.toggle_fav("friend/shared");
        a.cfg.repos.toggle_hidden("hello-org/infra");
        for (w, h) in [(80, 24), (120, 40)] {
            let s = render_app(&a, w, h);
            for want in [
                "Repositories",
                "4 of 5",
                "loading 5",
                "sort: pushed",
                "octocat/hello-world",
                "friend/shared",
                "★",
                "stars",
            ] {
                assert!(s.contains(want), "{w}x{h} missing {want:?}\n{s}");
            }
            assert!(
                !s.contains("hello-org/infra"),
                "hidden repos are not listed"
            );
            assert!(
                !s.contains("[3] Files"),
                "full screen: the normal panels are gone"
            );
            // favorites first
            let (fav, other) = (
                s.find("friend/shared").unwrap(),
                s.find("octocat/hello-world").unwrap(),
            );
            assert!(fav < other, "{s}");
        }
        assert!(
            render_app(&a, 120, 40).contains("language"),
            "wide terminals add the language column"
        );
        assert!(!render_app(&a, 80, 24).contains("language"));
        key(&mut a, KeyCode::Enter); // leave the search box
        key(&mut a, KeyCode::Char('.'));
        let s = render_app(&a, 120, 40);
        assert!(
            s.contains("hello-org/infra") && s.contains("⊘") && s.contains("5 of 5"),
            "{s}"
        );
        key(&mut a, KeyCode::Esc);
        assert!(a.browser.is_none() && render_app(&a, 120, 40).contains("[3] Files"));
    }

    #[test]
    fn comments_render_as_markdown_cards() {
        for (w, h) in [(80, 24), (120, 40)] {
            let mut a = seeded();
            key(&mut a, KeyCode::Char(']')); // Checks
            key(&mut a, KeyCode::Char(']')); // Comments
            let s = render_app(&a, w, h);
            for want in [
                "alice",
                "[member]",
                "edited",
                "ago",
                "src/main.rs:1",
                "rename",
                "\u{2022} first",
                "\u{2022} second",
                "let x = 1;",
                "\u{1f44d}",
                "octocat",
                "diff (https://example.com/d)",
            ] {
                assert!(s.contains(want), "{w}x{h} missing {want:?}\n{s}");
            }
            assert!(
                !s.contains("**") && !s.contains("```"),
                "markup is rendered, not shown\n{s}"
            );
            assert!(!s.contains("hubot, monalisa"));
            key(&mut a, KeyCode::Char('l')); // detail focus
            key(&mut a, KeyCode::Char('e')); // reactions: names
            assert!(render_app(&a, w, h).contains("hubot, monalisa"), "{w}x{h}");
        }
    }

    #[test]
    fn long_comments_collapse_and_enter_expands() {
        let mut a = seeded();
        let pr = a.pr_item().cloned().unwrap();
        let long: String = (0..60).map(|i| format!("para {i}\n\n")).collect();
        let card = crate::gh::Card {
            author: "bot".into(),
            body: long,
            ..Default::default()
        };
        let e = Entry {
            head: "comment by bot".into(),
            body: String::new(),
            resolved: false,
            thread: None,
            card,
        };
        a.seed(pr, vec![(Tab::Comments, Data::Comments(vec![e]))]);
        key(&mut a, KeyCode::Enter); // drill in
        key(&mut a, KeyCode::Char(']')); // Checks
        key(&mut a, KeyCode::Char(']')); // Comments panel: the thread pane
        let s = render_app(&a, 100, 40);
        assert!(
            s.contains("more lines (Enter to expand)") && !s.contains("para 59"),
            "{s}"
        );
        key(&mut a, KeyCode::Enter);
        key(&mut a, KeyCode::Char('l'));
        render_app(&a, 100, 40); // the renderer reports how far the pane can scroll
        key(&mut a, KeyCode::Char('G'));
        let s = render_app(&a, 100, 40);
        assert!(
            !s.contains("Enter to expand") && s.contains("para 59"),
            "{s}"
        );
    }

    fn tall_card_app(paras: usize) -> App {
        let mut a = seeded();
        let pr = a.pr_item().cloned().unwrap();
        let long: String = (0..paras).map(|i| format!("para{i} words\n\n")).collect();
        let card = crate::gh::Card {
            author: "bot".into(),
            body: long,
            ..Default::default()
        };
        let e = Entry {
            head: "comment by bot".into(),
            body: String::new(),
            resolved: false,
            thread: None,
            card,
        };
        a.seed(pr, vec![(Tab::Comments, Data::Comments(vec![e]))]);
        key(&mut a, KeyCode::Char(']'));
        key(&mut a, KeyCode::Char(']')); // Comments tab
        key(&mut a, KeyCode::Char('l'));
        key(&mut a, KeyCode::Enter); // expand
        a
    }

    /// Regression: a selected comment taller than the pane used to snap between its top and bottom
    /// on alternate frames (window() flip-flop), tearing the screen and "losing" text at some sizes.
    #[test]
    fn tall_expanded_comment_is_stable_and_pageable() {
        let mut a = tall_card_app(100);
        let frames: Vec<String> = (0..4).map(|_| render_app(&a, 140, 45)).collect();
        assert!(
            frames.windows(2).all(|w| w[0] == w[1]),
            "frame changes with no input:\n{}\n---\n{}",
            frames[0],
            frames[1]
        );
        assert!(
            frames[0].contains("bot")
                && frames[0].contains("para0 words")
                && !frames[0].contains("para90"),
            "{}",
            frames[0]
        );
        for _ in 0..4 {
            a.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
            render_app(&a, 140, 45);
        }
        let s = render_app(&a, 140, 45);
        assert_eq!(s, render_app(&a, 140, 45), "stays put after paging");
        assert!(
            s.contains("para4") && !s.contains("para0 words"),
            "Ctrl-d scrolls inside the comment\n{s}"
        );
        for _ in 0..4 {
            a.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
            render_app(&a, 140, 45);
        }
        assert!(
            render_app(&a, 140, 45).contains("para0 words"),
            "and back up"
        );
    }

    #[test]
    fn card_text_survives_every_pane_width() {
        let mut a = seeded();
        let pr = a.pr_item().cloned().unwrap();
        let body = "> quoted alpha1 beta1 gamma1 delta1 epsilon1 zeta1 eta1 theta1 iota1 kappa1 lambda1\n>\n> second2 paragraph2 inside3 quote3 with `inline4 code4` and more5 words5\n\n- item6 one6 two6 three6 four6 five6 six6 seven6 eight6 nine6 ten6\n- [x] task7 item7 with8 words8 that9 wrap9 around9 the10 pane10\n\n| head11 | other11 |\n|---|---|\n| cell12 | cell13 words13 |\n\nplain14 ending14 paragraph14 here14 with15 several15 words15 to16 wrap16\n";
        let card = crate::gh::Card {
            author: "bot".into(),
            body: body.into(),
            ..Default::default()
        };
        let e = Entry {
            head: "c".into(),
            body: String::new(),
            resolved: false,
            thread: None,
            card,
        };
        a.seed(pr, vec![(Tab::Comments, Data::Comments(vec![e]))]);
        key(&mut a, KeyCode::Char(']'));
        key(&mut a, KeyCode::Char(']'));
        let words: Vec<&str> = body
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 4 && w.ends_with(|c: char| c.is_ascii_digit()))
            .collect();
        for w in 60..=200u16 {
            let s = render_app(&a, w, 120);
            let flat: String = s
                .chars()
                .filter(|c| {
                    !c.is_whitespace() && !"\u{258f}\u{258e}\u{2502}\u{2022}\u{2551}".contains(*c)
                })
                .collect();
            for word in &words {
                // wrapping may split a word across rows, so compare with the row breaks removed
                assert!(flat.contains(word), "width {w}: lost {word:?}\n{s}");
            }
        }
    }

    #[test]
    fn help_and_bar_follow_remapped_keys() {
        let mut a = seeded();
        let keys: std::collections::BTreeMap<String, crate::config::Keys> = [
            ("actions".to_string(), crate::config::Keys::One("z".into())),
            ("help".to_string(), crate::config::Keys::One("!".into())),
            (
                "repo_browser".to_string(),
                crate::config::Keys::Many(vec!["b".into(), "ctrl-b".into()]),
            ),
        ]
        .into();
        a.keys = crate::config::Keymap::build(&keys).unwrap();
        let bar = render_app(&a, 160, 30).lines().last().unwrap().to_string();
        assert!(
            bar.contains("z actions") && bar.contains("! help") && bar.contains("b repos"),
            "{bar}"
        );
        assert!(
            !bar.contains("x actions") && !bar.contains("? help"),
            "{bar}"
        );
        key(&mut a, KeyCode::Char('!')); // the new help key works...
        let s = render_app(&a, 200, 60);
        assert!(
            s.contains("z  action menu") && s.contains("b / ctrl-b  repo browser"),
            "{s}"
        );
        assert!(
            s.contains("!  this help")
                && !s.contains("x  action menu")
                && !s.contains("?  this help"),
            "{s}"
        );
        assert!(
            s.contains("e  show who reacted")
                && s.contains("Ctrl-d/u half page (inside a tall comment first)"),
            "{s}"
        );
    }

    #[test]
    fn thread_headers_keep_time_and_badges_visible() {
        let mut a = seeded();
        let pr = a.pr_item().cloned().unwrap();
        let card = crate::gh::Card {
            author: "a-reviewer-with-a-rather-long-login".into(),
            role: "COLLABORATOR".into(),
            when: "2026-01-02T00:00:00Z".into(),
            edited: true,
            resolved: true,
            outdated: true,
            badge:
                "pkg/cmd/extension/browse/internal/some/deeply/nested/directory/model_test.go:218"
                    .into(),
            body: "nit".into(),
            ..Default::default()
        };
        let e = Entry {
            head: "thread".into(),
            body: "nit".into(),
            resolved: true,
            thread: Some("T".into()),
            card,
        };
        a.seed(pr, vec![(Tab::Comments, Data::Comments(vec![e]))]);
        key(&mut a, KeyCode::Char(']'));
        key(&mut a, KeyCode::Char(']'));
        let s = render_app(&a, 80, 24);
        for want in [
            "ago",
            "edited",
            "[resolved]",
            "[outdated]",
            "[collaborator]",
            "model_test.go:218",
            "\u{2026}",
        ] {
            assert!(s.contains(want), "missing {want:?}\n{s}");
        }
    }

    #[test]
    fn middle_ellipsis_never_cuts_the_start() {
        assert_eq!(mid_ellipsis("short.rs", 20, "…"), "short.rs");
        assert_eq!(mid_ellipsis("abcdefghij", 7, "…"), "abc…hij");
        assert_eq!(
            mid_ellipsis("long-document-name.md", 12, "…"),
            "long-d…me.md"
        );
        assert_eq!(mid_ellipsis("日本語日本語", 7, "…"), "日…語");
        assert!(mid_ellipsis("anything-long", 1, "…") == "…");
        assert_eq!(mid_ellipsis("abcdefghij", 7, "~"), "abc~hij");
        for w in 2..14 {
            let t = mid_ellipsis("README-long-name.md", w, "…");
            assert!(t.starts_with('R') && unicode_width::UnicodeWidthStr::width(t.as_str()) <= w);
        }
    }

    #[test]
    fn slicing_keeps_styles_across_wrapped_rows() {
        let hi = Style::new().bg(Color::Red);
        let spans = vec![Span::raw("abcd"), Span::styled("efgh", hi), Span::raw("ij")];
        let row2 = slice_spans(&spans, 3, 7);
        assert_eq!(
            row2.iter()
                .map(|s| s.content.to_string())
                .collect::<Vec<_>>(),
            ["d", "efg"]
        );
        assert_eq!(row2[1].style, hi);
    }

    #[test]
    fn tiny_terminal_and_ascii() {
        let s = render(40, 10, false, IconSet::Unicode);
        assert!(s.contains("Terminal too small") && !s.contains('╭'), "{s}");
        let s = render(100, 30, false, IconSet::Ascii);
        assert!(
            s.contains("+-") && !s.contains('╭') && !s.contains('✓'),
            "{s}"
        );
    }

    /// Perf guard: `cargo test --release perf_5k -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn perf_5k_line_diff_frame() {
        let mut d = String::from(
            "diff --git a/big.md b/big.md\n--- a/big.md\n+++ b/big.md\n@@ -1,5000 +1,5000 @@\n",
        );
        for i in 0..2500 {
            d += &format!("-old line {i} {LONG} {LONG}\n+new line {i} {LONG} {LONG} tail\n");
        }
        let mut a = seeded();
        let files = crate::diff::parse(&d);
        let pr = a.pr_item().cloned().unwrap();
        a.seed(
            pr,
            vec![(
                Tab::Diff,
                Data::Files(FilesData {
                    sha: "x".into(),
                    files,
                    threads: Default::default(),
                }),
            )],
        );
        key(&mut a, KeyCode::Char('3'));
        key(&mut a, KeyCode::Char('l'));
        for _ in 0..12 {
            key(&mut a, KeyCode::Char('G')); // cursor to the last row: worst case for offsets
        }
        for (w, mode) in [(120u16, "unified/wrap"), (220, "split/wrap")] {
            let t = std::time::Instant::now();
            for _ in 0..10 {
                render_app(&a, w, 40);
            }
            println!("{mode}: {:?} per frame", t.elapsed() / 10);
        }
    }
}
