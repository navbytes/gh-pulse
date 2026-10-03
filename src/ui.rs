use crate::app::{App, Hit, Load, Modal, PK};
use crate::browse::{self, Browser};
use crate::config::Act;
use crate::diff::{self, DLine, DiffMode, Op, Row};
use crate::gh::{self, Data, FilesData, Item, Kind, Tab};
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
Panels (default: Pull requests, Files, Issues, Actions, Repo; choose them in [panels])
  1-5, Tab/S-Tab  focus panel (its number again: next list tab)   {{ }}  previous/next list tab
  j/k, arrows     move             Ctrl-d/u  half page     g/G or Home/End  top/bottom
  {filter}  filter (Enter apply, Esc clear)
  l/Right         focus the detail pane   h/Esc/Left  back to the list
  {global}  (list focus) switch between the repo and the global home; in Files and PR drill-in it jumps to the last row
  {browser}  repo browser: all your repos (search, sort, favorite, hide, Enter switches)
  {inbox}  inbox: unread notifications of all repos (Enter opens it here, m marks read, o browser)
Global home (opens outside a repo or with --start global; [panels] global picks the sections)
  Review requested, My PRs, Issues, Repos: searches across all your repos, newest update first
  {scope}  scope: all / favorites / an org / one repo      {switch}  open the selected item's repo (G returns)
  Repos panel: Enter open the repo   {scope} scope the home to it   {zoom} favorite   H hide
Repo panel (Branches / Tags / Releases tabs)
  Enter/l  details (a tag: commit, date, release)   {copy} on a tag copies its name   {actions}  branch/release actions
Pull requests
  Files panel follows the selected PR; j/k there changes the file shown in the diff
  Enter on a PR drills in: Files / Commits / Checks / Comments of that PR (Esc returns)
  Commits: j/k picks a commit, the right pane shows its diff (n/p file, t/w/{zoom} as in Diff)
  [ ]  detail tab (Overview/Checks/Comments/Diff/Commits); in a PR drill-in: next/prev panel
Diff
  j/k line   Ctrl-d/u half page   g/G first/last row   n/p file   t unified/split/auto
  w wrap/clip   v mark file viewed   {zoom} zoom
Comments
  j/k move by comment   Ctrl-d/u half page (inside a tall comment first)
  Enter  expand/collapse (long comments, <details>)   e  show who reacted (asks GitHub once per comment)
Actions (always asks to confirm, shows the exact command)
  {actions}  action menu for the selected item / row    {approve} approve  {comment} comment  {merge} merge
  n  new issue (Issues panel) / new PR from the cwd branch (PR panel)   d  run workflow (Actions panel)
  Forms: Tab/S-Tab fields, Space toggles, Left/Right pickers, Ctrl-S review the command, Esc cancel
  Input popups: Enter newline, Ctrl-S submit, Esc cancel
Mouse: click panel/row/tab, wheel scrolls
Anywhere
  {open}  open in browser    {copy}  copy URL    {checkout}  checkout selected PR
  {refresh}  refresh the selected item and its list      {refresh_all}  reload everything (skips the cache)    {log}  command log
  {help}  this help    {quit}  quit
(remap the keys marked above in config.toml, see docs/configuration.md)",
        filter = k(Act::Filter),
        global = k(Act::Global),
        browser = k(Act::Browser),
        inbox = k(Act::Inbox),
        scope = k(Act::Scope),
        switch = k(Act::SwitchRepoContext),
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
    if app.inbox.is_some() {
        inbox_view(f, app, area);
        return modal(f, app);
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
    // the badge keeps the right edge; hidden at 0 and when the fetch failed
    let badge = app.unread_count().filter(|n| *n > 0).map(|n| {
        format!(
            "{} {} ",
            th.ic.mail,
            if n >= 100 {
                "99+".into()
            } else {
                n.to_string()
            }
        )
    });
    let bw = badge.as_ref().map_or(0, |b| {
        unicode_width::UnicodeWidthStr::width(b.as_str()) as u16
    });
    // `⚡ 412/5000` when the quota is low, left of the badge
    let chip = app
        .quota_chip()
        .map(|c| format!("{} {}/{} ", th.ic.zap, c.remaining, c.limit));
    let cw = chip.as_ref().map_or(0, |c| {
        unicode_width::UnicodeWidthStr::width(c.as_str()) as u16
    });
    let [head, chip_area, badge_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(cw),
        Constraint::Length(bw),
    ])
    .areas(head);
    if let Some(c) = chip {
        f.render_widget(
            Paragraph::new(Span::styled(c, Style::new().fg(th.err).bold())),
            chip_area,
        );
    }
    // repo, branch and user always; the repo facts are dropped from the right as the width shrinks
    let mut facts = vec![];
    if app.ctx.is_none()
        && let Some(m) = &app.meta
    {
        facts.push(format!("{} {}", th.ic.star, m.stars));
        facts.push(if m.private { "private" } else { "public" }.to_string());
        if !m.branch.is_empty() {
            facts.push(format!("default {}", m.branch));
        }
        if let Some((n, more)) = app.open_prs() {
            facts.push(open_prs_fact(n, more));
        }
    }
    let extra = if loading { 4 } else { 0 };
    let avail = (head.width as usize).saturating_sub(extra);
    let (title, tail) = fit_header(&title, &facts, th.ic.dot, th.ic.ell, avail);
    let mut hdr = vec![Span::styled(title, Style::new().fg(th.accent).bold())];
    if !tail.is_empty() {
        hdr.push(Span::styled(tail, Style::new().fg(th.muted)));
    }
    if loading {
        hdr.push(Span::styled(
            format!("  {}", spinner(app)),
            Style::new().fg(th.muted),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(hdr)), head);
    if let Some(b) = badge {
        f.render_widget(
            Paragraph::new(Span::styled(b, Style::new().fg(th.warn).bold())),
            badge_area,
        );
    }

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
    let n = (0..app.panels.len()).filter(|&i| !app.collapsed(i)).count() as u16;
    let compact = left.height < 3 * n.saturating_sub(1) + 5;
    let rows = (0..app.panels.len()).map(|i| match (i == app.focus, compact) {
        _ if app.collapsed(i) => Constraint::Length(1),
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

/// `sha7 subject        +a -d` for the Commits panel.
fn commit_label(app: &App, it: &Item, w: usize) -> Line<'static> {
    use unicode_width::UnicodeWidthStr;
    let th = &app.theme;
    let (adds, dels, _) = it.fm;
    let right = vec![
        Span::styled(format!(" +{adds}"), Style::new().fg(th.ok)),
        Span::styled(format!(" -{dels}"), Style::new().fg(th.err)),
    ];
    let rw = Line::from(right.clone()).width();
    let budget = w.saturating_sub(it.state.width() + 1 + rw + 1);
    let subject = mid_ellipsis(&it.title, budget, th.ic.ell);
    let mut v = vec![
        Span::styled(format!("{} ", it.state), Style::new().fg(th.accent)),
        Span::raw(subject),
    ];
    let used = Line::from(v.clone()).width();
    v.push(Span::raw(" ".repeat(w.saturating_sub(used + rw))));
    v.extend(right);
    Line::from(v)
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
    let viewed = app.is_viewed(&it.title);
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

/// Right pane when nothing is selected: what the repo is, from the header's lookup.
fn repo_overview(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let Some(m) = &app.meta else {
        return f.render_widget(bordered(app, " Detail ".into(), false), area);
    };
    let block = bordered(app, " Overview ".into(), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let muted = Style::new().fg(th.muted);
    let kv = |k: &str, v: String| {
        Line::from(vec![Span::styled(format!("{k:<16}"), muted), Span::raw(v)])
    };
    let mut lines = vec![Line::styled(app.repo.clone(), Style::new().bold())];
    lines.extend(
        wrap(&m.description, inner.width as usize)
            .into_iter()
            .map(Line::raw),
    );
    lines.push(Line::raw(""));
    lines.push(kv(
        "visibility",
        if m.private { "private" } else { "public" }.into(),
    ));
    lines.push(kv("stars", m.stars.to_string()));
    lines.push(kv("forks", m.forks.to_string()));
    lines.push(kv("open issues", m.issues.to_string()));
    lines.push(kv("default branch", m.branch.clone()));
    if !m.license.is_empty() {
        lines.push(kv("license", m.license.clone()));
    }
    if !m.pushed.is_empty() {
        lines.push(kv("last push", m.pushed.clone()));
    }
    if !m.topics.is_empty() {
        lines.push(kv("topics", m.topics.join(", ")));
    }
    f.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

/// The header text for `avail` columns: `base` whole (ellipsized only if it alone is too long), then as
/// many of `facts` as fit, whole, dropping from the right.
fn open_prs_fact(n: usize, more: bool) -> String {
    let plus = if more { "+" } else { "" };
    format!(
        "{n}{plus} open PR{}",
        if n == 1 && !more { "" } else { "s" }
    )
}

fn fit_header(
    base: &str,
    facts: &[String],
    dot: &str,
    ell: &str,
    avail: usize,
) -> (String, String) {
    use unicode_width::UnicodeWidthStr;
    if base.width() > avail {
        let keep: String = base.chars().take(avail.saturating_sub(1)).collect();
        return (format!("{keep}{ell}"), String::new());
    }
    let (mut tail, mut used) = (String::new(), base.width());
    for (i, f) in facts.iter().enumerate() {
        let piece = if i == 0 {
            format!("  {f}")
        } else {
            format!(" {dot} {f}")
        };
        if used + piece.width() > avail {
            break;
        }
        used += piece.width();
        tail += &piece;
    }
    (base.to_string(), tail)
}

/// Full-screen unread-notifications view (`N`).
fn inbox_view(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let Some(ib) = &app.inbox else { return };
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    let title = format!(" Inbox  {} unread", ib.items.len());
    let mut hdr = vec![Span::styled(title, Style::new().fg(th.accent).bold())];
    if ib.loading {
        hdr.push(Span::styled(
            format!("  {}", spinner(app)),
            Style::new().fg(th.muted),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(hdr)), head);
    let block = bordered(app, " Notifications (all repos) ".into(), true);
    if let Some(e) = &ib.error {
        return f.render_widget(
            Paragraph::new(note(app, Note::Err(e.clone()))).block(block),
            body,
        );
    }
    if ib.items.is_empty() {
        let n = if ib.loading {
            Note::Loading
        } else {
            Note::Empty("no unread notifications")
        };
        return f.render_widget(Paragraph::new(note(app, n)).block(block), body);
    }
    let muted = Style::new().fg(th.muted);
    let rows = ib.items.iter().map(|it| {
        let what = match it.kind {
            Kind::Pr => "PR",
            Kind::Issue => "issue",
            _ => "other",
        };
        let num = if it.number > 0 {
            format!("#{} ", it.number)
        } else {
            String::new()
        };
        // meta is "repo · reason"
        let reason = it.meta.split_once(" \u{b7} ").map_or("", |(_, r)| r);
        ListItem::new(Line::from(vec![
            Span::styled(format!("{} ", th.ic.unread), Style::new().fg(th.accent)),
            Span::styled(format!("{what:<5} "), muted),
            Span::styled(format!("{} ", it.repo_display()), muted),
            Span::styled(num, muted),
            Span::raw(it.title.clone()),
            Span::styled(format!("  {reason}"), muted),
        ]))
    });
    let list = List::new(rows)
        .block(block)
        .highlight_style(Style::new().bg(th.sel_bg).bold());
    let mut st = ListState::default().with_selected(Some(ib.cursor.min(ib.items.len() - 1)));
    f.render_stateful_widget(list, body, &mut st);
    let hint = format!(
        "j/k move  Enter open  m mark read  o browser  r refresh  {}/Esc back",
        app.keys.label(Act::Inbox)
    );
    let status = if app.status.is_empty() {
        hint
    } else {
        app.status.clone()
    };
    f.render_widget(
        Paragraph::new(Span::styled(status, Style::new().fg(th.muted))),
        foot,
    );
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
    if let Some(n) = app.pause_note() {
        spans.push(Span::styled(format!("{n}  "), Style::new().fg(th.warn)));
    }
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
            PK::Commits => format!(
                "j/k commit  l/Enter diff  n/p file  t mode  w wrap  {zoom} zoom  {back}[ ] panel"
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
    } else if app.panels[app.focus].kind == PK::Repos {
        format!(
            "j/k move  Enter open repo  {} scope  {zoom} favorite  H hide  {} repos",
            kl(Act::Scope),
            kl(Act::Browser)
        )
    } else if app.global {
        format!(
            "j/k move  Enter drill in  [ ] detail tab  {filter} filter  {} scope  {} open repo  {} repos  {} inbox  {actions} actions",
            kl(Act::Scope),
            kl(Act::SwitchRepoContext),
            kl(Act::Browser),
            kl(Act::Inbox)
        )
    } else {
        format!(
            "j/k move  Enter drill in  [ ] detail tab  {{ }} list tab  {filter} filter  {actions} actions  {} repos  {} inbox",
            kl(Act::Browser),
            kl(Act::Inbox)
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

/// Focus/rail bar; the block glyph does not exist in ASCII mode.
fn bar(th: &Theme) -> &'static str {
    if th.ascii { "|" } else { "\u{258e}" }
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
    let prefixed = matches!(it.kind, Kind::Pr | Kind::Issue);
    let repo = it.repo_display();
    if show_repo && !repo.is_empty() && !prefixed && it.kind != Kind::Repo {
        v.push(Span::styled(format!("{repo} "), muted));
    }
    match it.kind {
        Kind::Pr | Kind::Issue => {
            // `owner/repo#N` with the owner dimmed, when rows from several repos share a list
            match repo.split_once('/').filter(|_| show_repo) {
                Some((owner, name)) => {
                    v.push(Span::styled(format!("{owner}/"), muted));
                    v.push(Span::raw(name.to_string()));
                    v.push(Span::styled(format!("#{} ", it.number), muted));
                }
                None => v.push(Span::styled(format!("#{} ", it.number), muted)),
            }
            if it.is_draft {
                v.push(Span::styled("[draft] ", muted));
            }
            v.push(Span::raw(it.title.clone()));
            if show_repo && matches!(it.state.to_lowercase().as_str(), "merged" | "closed") {
                v.push(Span::styled(
                    format!(" [{}]", it.state.to_lowercase()),
                    muted,
                ));
            }
        }
        Kind::Repo => {
            let (owner, name) = repo.split_once('/').unwrap_or(("", &repo));
            let star = if it.state == "fav" { th.ic.star } else { " " };
            v.push(Span::styled(format!("{star} "), Style::new().fg(th.warn)));
            v.push(Span::styled(format!("{owner}/"), muted));
            v.push(Span::raw(name.to_string()));
            if !it.meta.is_empty() {
                v.push(Span::styled(format!("  {}", it.meta), muted));
            }
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
        Kind::Tag => {
            v.push(Span::raw(it.title.clone()));
            v.push(Span::styled(format!("  {}", it.meta), muted));
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
        Kind::Commit => return commit_label(app, it, w),
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

/// One piece of a panel title; `tab` marks the clickable list-tab labels.
struct Part {
    text: String,
    tab: Option<usize>,
    active: bool,
}

fn count_txt(app: &App, c: Option<(usize, bool)>, wanted: bool, failed: bool) -> String {
    match c {
        None if failed => app.theme.ic.fail.to_string(),
        // `…` while a count is on its way, `?` when it is only fetched once the tab is opened
        None if wanted => app.theme.ic.ell.to_string(),
        None => "?".to_string(),
        Some((n, true)) => format!("{n}+"),
        Some((n, false)) => n.to_string(),
    }
}

/// `[5] Branches 32 · Tags 14 · Releases 81`: every list tab with its count (`…` until known).
fn tab_title(app: &App, i: usize, short_name: bool, short_tabs: bool) -> Vec<Part> {
    let (p, th) = (&app.panels[i], &app.theme);
    let plain = |text: String| Part {
        text,
        tab: None,
        active: false,
    };
    let mut v = vec![plain(format!(" [{}] ", i + 1))];
    if !p.title.is_empty() {
        let name = if short_name && p.title == "Pull requests" {
            "PRs"
        } else {
            p.title
        };
        v.push(plain(format!("{name} ")));
    }
    for (ti, t) in p.tabs.iter().enumerate() {
        if ti > 0 {
            v.push(plain(format!(" {} ", th.ic.dot)));
        }
        let label = if short_tabs { t.short } else { t.label };
        v.push(Part {
            text: format!(
                "{label} {}",
                count_txt(
                    app,
                    app.tab_count(i, ti),
                    app.count_wanted(i, ti),
                    app.tab_failed(i, ti)
                )
            ),
            tab: Some(ti),
            active: ti == p.tab,
        });
    }
    v.push(plain(extra_note(app, i, short_tabs)));
    v
}

/// ` · 3 hidden` / ` · +4 favorites not shown` after the tabs of a global section (full labels only).
fn extra_note(app: &App, i: usize, squeezed: bool) -> String {
    let p = &app.panels[i];
    let mut s = String::new();
    if !squeezed {
        if p.hidden > 0 {
            s += &format!(" {} {} hidden", app.theme.ic.dot, p.hidden);
        }
        if p.not_shown > 0 {
            s += &format!(" {} +{} favorites not shown", app.theme.ic.dot, p.not_shown);
        }
    }
    s + " "
}

/// The one-tab form: `[2] Files (12)`, `[1] Pull requests · Mine (3)`; `with_tab` keeps the tab name,
/// `short_name` squeezes the panel name to `PRs` (or drops it).
fn simple_title(app: &App, i: usize, with_tab: bool, short_name: Option<bool>) -> Vec<Part> {
    let (p, th) = (&app.panels[i], &app.theme);
    let tab = p.tabs.get(p.tab).map(|t| t.label);
    let name = if p.title.is_empty() {
        tab.unwrap_or("")
    } else {
        p.title
    };
    let name = match short_name {
        None => String::new(),
        Some(true) if name == "Pull requests" => "PRs ".to_string(),
        Some(_) if name.is_empty() => String::new(),
        Some(_) => format!("{name} "),
    };
    let extra = match (with_tab, p.title.is_empty(), tab) {
        (true, false, Some(t)) => format!("{} {t} ", th.ic.dot),
        _ => String::new(),
    };
    let count = match (p.kind, app.active_count(i)) {
        (PK::Status, _) => String::new(),
        (_, Some((n, true))) => format!("({n}+)"),
        (_, Some((n, false))) => format!("({n})"),
        (_, None) if app.tab_failed(i, p.tab) => format!("({})", th.ic.fail),
        // `…` while a count is on its way, `?` when it is only fetched once the tab is opened
        (_, None) if app.count_wanted(i, p.tab) => format!("({})", th.ic.ell),
        (_, None) => "(?)".to_string(),
    };
    let note = if with_tab && short_name == Some(false) {
        extra_note(app, i, false)
    } else {
        " ".to_string()
    };
    let text = format!(" [{}] {name}{extra}{count}{note}", i + 1);
    vec![Part {
        text,
        tab: None,
        active: false,
    }]
}

fn parts_width(v: &[Part]) -> usize {
    use unicode_width::UnicodeWidthStr;
    v.iter().map(|p| p.text.width()).sum()
}

/// The fullest title form that fits `avail` columns, degrading by whole segments (every tab with its
/// count, short tab names, the showing tab, the panel name, `PRs`, the number alone); a count is
/// never cut in half. Only a width under the last form gets an ellipsis.
fn fit_title(app: &App, i: usize, avail: usize) -> Vec<Part> {
    let mut forms = vec![];
    if app.panels[i].tabs.len() > 1 {
        forms.push(tab_title(app, i, false, false));
        forms.push(tab_title(app, i, true, false));
        forms.push(tab_title(app, i, true, true));
    }
    forms.push(simple_title(app, i, true, Some(false)));
    forms.push(simple_title(app, i, false, Some(false)));
    forms.push(simple_title(app, i, false, Some(true)));
    forms.push(simple_title(app, i, false, None));
    if let Some(f) = forms.iter().position(|f| parts_width(f) <= avail) {
        return forms.swap_remove(f);
    }
    let mut last = forms.pop().unwrap_or_default();
    if let Some(p) = last.first_mut() {
        let keep: String = p.text.chars().take(avail.saturating_sub(1)).collect();
        p.text = format!("{keep}{}", app.theme.ic.ell);
    }
    last
}

fn panel(f: &mut Frame, app: &App, i: usize, area: Rect, compact: bool) {
    use unicode_width::UnicodeWidthStr;
    let th = &app.theme;
    let p = &app.panels[i];
    let focused = i == app.focus;
    let items = app.visible(i);
    let collapsed = app.collapsed(i);
    if collapsed {
        let name = if p.title.is_empty() { "Repo" } else { p.title };
        let line = Line::styled(
            format!(" [{}] {name} (empty)", i + 1),
            Style::new().fg(th.muted).italic(),
        );
        return f.render_widget(Paragraph::new(line), area);
    }
    let borderless = compact && !focused;
    let avail = (area.width as usize).saturating_sub(if borderless { 0 } else { 2 });
    let parts = fit_title(app, i, avail);
    // clickable tab labels start right after the corner (or at the edge of a borderless line)
    let mut x = area.x + u16::from(!borderless);
    let (mut spans, mut ptabs) = (vec![], vec![]);
    for part in &parts {
        let w = part.text.width() as u16;
        let st = match (part.tab, part.active) {
            (Some(_), true) => Style::new().fg(th.accent).bold().underlined(),
            (Some(_), false) => Style::new().fg(th.muted).not_bold(),
            _ => Style::default(),
        };
        if let Some(t) = part.tab {
            ptabs.push(crate::app::PTab {
                y: area.y,
                x0: x,
                x1: x + w,
                panel: i,
                tab: t,
            });
        }
        x += w;
        spans.push(Span::styled(part.text.clone(), st));
    }
    app.hit.borrow_mut().ptabs.extend(ptabs);
    if borderless {
        let line = Line::from(spans).style(Style::new().fg(th.muted));
        return f.render_widget(Paragraph::new(line), area);
    }
    let title = Line::from(spans);
    let block = bordered_line(app, title, focused && !app.detail_focus);
    let inner = block.inner(area);

    let cur = items.get(p.cursor.min(items.len().saturating_sub(1)));
    let (Some(_), None) = (cur, &p.error) else {
        let n = match &p.error {
            Some(e) => Note::Err(e.clone()),
            None if p.loading => Note::Loading,
            None if p.unloaded => Note::Empty("loads when you focus it"),
            None if p.kind.derived() && app.pr_item().is_none() => {
                Note::Empty("select a pull request")
            }
            None if p.kind.is_global_search() && app.favorites_missing() => Note::Empty(
                "No favorites yet \u{2014} press f in the Repos panel or B (repo browser) to add some",
            ),
            None => Note::Empty(match p.kind {
                PK::Files => "no files",
                PK::Commits => "no commits",
                PK::Checks => "no checks",
                PK::Comments => "no comments",
                _ => "nothing here",
            }),
        };
        return f.render_widget(
            Paragraph::new(note(app, n))
                .wrap(ratatui::widgets::Wrap { trim: true })
                .block(block),
            area,
        );
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
        (PK::Commits, Some(i)) => format!(" Commit {} {} ", i.state, i.title),
        (PK::Commits, None) => " Commit ".to_string(),
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
    if k == PK::Commits {
        match (app.data_of(it, Tab::Diff), app.diff_data()) {
            (_, Some((fd, fi))) => {
                diff_view(f, app, fd, inner, fi, &format!("commit|{}", it.meta), false)
            }
            (Some(Load::Done(Err(e))), _) => {
                f.render_widget(Paragraph::new(note(app, Note::Err(e.clone()))), inner)
            }
            _ => f.render_widget(Paragraph::new(note(app, Note::Loading)), inner),
        }
    } else if k == PK::Checks {
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
    if let Some(k @ (PK::Checks | PK::Comments | PK::Commits)) = app.dk() {
        return derived_detail(f, app, area, k);
    }
    let Some(it) = app.selected() else {
        return repo_overview(f, app, area);
    };
    if it.kind == Kind::Repo {
        let block = bordered(app, " Repo ".into(), app.detail_focus);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let lines: Vec<Line> = it
            .body
            .lines()
            .enumerate()
            .flat_map(|(i, l)| {
                wrap(l, inner.width as usize)
                    .into_iter()
                    .map(move |r| (i, r))
            })
            .map(|(i, r)| {
                if i == 0 {
                    Line::styled(r, Style::new().bold())
                } else {
                    Line::raw(r)
                }
            })
            .collect();
        return f.render_widget(Paragraph::new(lines), inner);
    }
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
    if tab != Tab::Overview || matches!(it.kind, Kind::Status | Kind::Release | Kind::Tag) {
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
        (Tab::Comments, Some(Data::Comments(cd))) => {
            let e = &cd.entries;
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
            // Paging status goes on top where it is seen: the rest of the thread loads in the background.
            let status = if cd.loading {
                Some(Line::from(vec![
                    Span::styled(spinner(app), Style::new().fg(th.accent)),
                    Span::styled(
                        format!(
                            " loading more\u{2026} ({}/{})  {} more",
                            cd.loaded,
                            cd.total,
                            cd.remaining()
                        ),
                        Style::new().fg(th.muted),
                    ),
                ]))
            } else if let Some(e) = &cd.error {
                let wait = cd.wait_secs();
                let text = if wait > 0 {
                    format!(
                        "{} GitHub rate limit \u{2014} retry in {wait}s (m retries)",
                        th.ic.fail
                    )
                } else {
                    format!(
                        "{} {} more not loaded: {e}  (m to retry)",
                        th.ic.fail,
                        cd.remaining()
                    )
                };
                Some(Line::styled(text, Style::new().fg(th.err)))
            } else if cd.paused() {
                Some(Line::styled(
                    format!(
                        "{} more not loaded yet ({}/{}): scroll down or press m",
                        cd.remaining(),
                        cd.loaded,
                        cd.total
                    ),
                    Style::new().fg(th.muted),
                ))
            } else {
                None
            };
            if let Some(l) = status {
                lines.insert(0, l);
                sel = (sel.0 + 1, sel.1 + 1);
                starts.iter_mut().for_each(|s| *s += 1);
            }
            *app.row_starts.borrow_mut() = starts;
            pane(f, app, inner, lines, Some(sel))
        }
        (Tab::Diff, Some(Data::Files(fd))) => {
            diff_view(f, app, fd, inner, app.file(), &app.pr_key(), true)
        }
        (Tab::Commits, Some(Data::Commits(c))) => {
            let now = now_secs();
            let lines = c
                .iter()
                .map(|c| {
                    let sha: String = c.sha.chars().take(7).collect();
                    Line::from(vec![
                        Span::styled(format!("{sha} "), Style::new().fg(th.accent)),
                        Span::raw(c.subject.clone()),
                        Span::styled(
                            format!(
                                "  {} {} {} ago ",
                                c.author,
                                th.ic.dot,
                                crate::browse::ago(&c.when, now)
                            ),
                            Style::new().fg(th.muted),
                        ),
                        Span::styled(format!("+{}", c.adds), Style::new().fg(th.ok)),
                        Span::styled(format!(" -{}", c.dels), Style::new().fg(th.err)),
                    ])
                })
                .collect();
            pane(f, app, inner, lines, None)
        }
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
        hl.ensure(
            &key,
            &format!("x.{}", lang_ext(lang)),
            &lines,
            0,
            lines.len(),
        );
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
            // names come with a second call after `e`; until then the count stands
            if open && !r.users.is_empty() {
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
    let rail = |on: bool| Span::styled(if on { bar(th) } else { " " }, Style::new().fg(th.accent));
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
        Span::styled(format!("{}{id}  ", it.repo_display()), muted),
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

/// `fi` picks the file; `scope` namespaces the highlight cache; `track_viewed` is for PR files only.
fn diff_view(
    f: &mut Frame,
    app: &App,
    fd: &FilesData,
    area: Rect,
    fi: usize,
    scope: &str,
    track_viewed: bool,
) {
    let th = &app.theme;
    let files = &fd.files;
    let fi = fi.min(files.len().saturating_sub(1));
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
    let viewed = track_viewed && app.is_viewed(&file.path);
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
            "  [{mode}, {}]  t mode  w wrap{}",
            if app.wrap { "wrap" } else { "clip" },
            if track_viewed { "  v viewed" } else { "" }
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
    let mut h = body.height as usize;
    let mut top = window(app, h, total, sel);
    // Sticky hunk bar: once a hunk's header has scrolled off the top, pin it in the first row.
    let hunk_rows: Vec<usize> = match &rows {
        None => file
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.op == Op::Hunk)
            .map(|(i, _)| i)
            .collect(),
        Some(rs) => rs
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, Row::Full(_)))
            .map(|(i, _)| i)
            .collect(),
    };
    let sticky = |top: usize| {
        let r0 = starts.partition_point(|&s| s <= top).saturating_sub(1);
        hunk_rows
            .iter()
            .rev()
            .find(|&&hr| hr <= r0 && starts[hr] < top)
            .copied()
    };
    let mut pin = None;
    if sticky(top).is_some() && h > 2 {
        // the pinned row costs one body row, which can move the window: re-check with it
        let top2 = window(app, h - 1, total, sel);
        match sticky(top2) {
            Some(p) => (pin, top, h) = (Some(p), top2, h - 1),
            None => top = window(app, h, total, sel),
        }
    }
    let r0 = starts.partition_point(|&s| s <= top).saturating_sub(1);
    let mut r1 = r0;
    while r1 + 1 < starts.len() && starts[r1 + 1] < top + h {
        r1 += 1;
    }
    let from = match &rows {
        None => r0,
        Some(r) => r[r0..=r1.min(r.len().saturating_sub(1))]
            .iter()
            .map(|row| match row {
                Row::Full(i) => *i,
                Row::Pair(a, b) => a.iter().chain(b.iter()).copied().min().unwrap_or(0),
            })
            .min()
            .unwrap_or(0),
    };
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

    let key = format!("{scope}|{}", file.path);
    let mut guard = app.hl.borrow_mut();
    let hl = guard.get_or_insert_with(|| Hl::new(th.syntect_theme(), th.rgb_fn()));
    hl.ensure(&key, &file.path, &file.lines, from, upto);
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
    let mut content = body;
    if let Some(p) = pin {
        let (bar, rest) = (
            Rect { height: 1, ..body },
            Rect {
                y: body.y + 1,
                height: body.height - 1,
                ..body
            },
        );
        let hi = match &rows {
            None => p,
            Some(rs) => match &rs[p] {
                Row::Full(i) => *i,
                Row::Pair(..) => p,
            },
        };
        let bar_lines = paint.hunk(&file.lines[hi].text, g.w, false, false);
        f.render_widget(Paragraph::new(bar_lines), bar);
        content = rest;
        app.hit.borrow_mut().body = rest;
    }
    f.render_widget(Paragraph::new(lines), content);
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
        Modal::Scope(p) => {
            use crate::app::ScopeStage::*;
            let choices = app.scope_choices(p);
            let typing = p.stage != Top;
            let title = match p.stage {
                Top => "Scope (Enter pick, Esc cancel)",
                Orgs => "Scope: organization (type to filter, Backspace back)",
                Repos => "Scope: repo (type owner/name, Backspace back)",
            };
            let h = (choices.len().clamp(1, 12) + 2 + usize::from(typing)) as u16;
            let area = popup(f, app, 60, h, title);
            let [qa, la] =
                Layout::vertical([Constraint::Length(u16::from(typing)), Constraint::Min(0)])
                    .areas(area);
            if typing {
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled("> ", Style::new().fg(th.accent)),
                        Span::raw(format!("{}_", p.query)),
                    ])),
                    qa,
                );
            }
            let rows: Vec<ListItem> = if choices.is_empty() {
                let msg = match (p.stage, &app.orgs) {
                    (Orgs, None) => "loading your organizations...",
                    (Orgs, Some(o)) if o.is_empty() && p.query.is_empty() => {
                        "you are not in any organization"
                    }
                    _ => "no match",
                };
                vec![ListItem::new(Span::styled(msg, Style::new().fg(th.muted)))]
            } else {
                choices
                    .iter()
                    .map(|(l, c)| {
                        let cur = matches!(c, crate::app::ScopeChoice::Set(s) if *s == app.scope);
                        ListItem::new(format!("{}{l}", if cur { "\u{25cf} " } else { "  " }))
                    })
                    .collect()
            };
            f.render_stateful_widget(
                List::new(rows).highlight_style(bar),
                la,
                &mut ListState::default()
                    .with_selected(Some(p.cursor.min(choices.len().saturating_sub(1)))),
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
            // Always the full command: grow to fit; when the terminal is too small, scroll, and `y`
            // stays refused until the end has been seen.
            let w = f.area().width.saturating_sub(4).clamp(20, 100);
            let mut lines = wrap(&gh::shell(&c.cmd), w as usize - 2);
            if let Some(body) = &c.stdin {
                lines.push(format!("(the {}-byte body is piped on stdin)", body.len()));
            }
            let h = (lines.len() as u16 + 2).min(f.area().height.saturating_sub(2));
            let tall = lines.len() as u16 + 2 > h;
            let hint = if tall { "j/k PgDn G scroll, " } else { "" };
            let area = popup(
                f,
                app,
                w,
                h,
                &format!("Run this command?  [{hint}y yes, n/Esc no]"),
            );
            // a tall command keeps its last row for the "more" marker
            let view = if tall {
                area.height.saturating_sub(1)
            } else {
                area.height
            };
            c.max_scroll.set((lines.len() as u16).saturating_sub(view));
            c.view_h.set(view);
            let scroll = c.scroll.min(c.max_scroll.get());
            let [body, foot] =
                Layout::vertical([Constraint::Length(view), Constraint::Min(0)]).areas(area);
            let lines: Vec<Line> = lines.into_iter().map(Line::raw).collect();
            f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), body);
            if tall {
                let m = if scroll < c.max_scroll.get() {
                    format!("{} more (command continues: scroll)", th.ic.down)
                } else {
                    format!("{} end of command", th.ic.up)
                };
                f.render_widget(
                    Paragraph::new(Span::styled(m, Style::new().fg(th.warn).bold())),
                    foot,
                );
            }
        }
        Modal::Form(form) => form_popup(f, app, form),
    }
}

/// The create-issue / create-PR form: one block per field, the focused one highlighted.
fn form_popup(f: &mut Frame, app: &App, form: &crate::form::Form) {
    use crate::form::Kind;
    let th = &app.theme;
    let w = f.area().width.saturating_sub(4).clamp(30, 100);
    let inner_w = w.saturating_sub(4) as usize;
    let show = |s: &str| crate::sanitize::clean(s).into_owned();
    // an input's description, shown after its label
    let hint = |fl: &crate::form::Field| {
        if fl.hint.is_empty() {
            String::new()
        } else {
            format!("  {}", show(&fl.hint))
        }
    };
    let mut lines: Vec<Line> = vec![];
    if let Some(n) = &form.note {
        for r in wrap(&show(n), inner_w) {
            lines.push(Line::styled(r, Style::new().fg(th.muted).italic()));
        }
    }
    let mut focus_at = 0;
    for (i, fl) in form.fields.iter().enumerate() {
        let on = i == form.focus;
        if on {
            focus_at = lines.len();
        }
        let label_style = if on {
            Style::new().fg(th.accent).bold()
        } else {
            Style::new().fg(th.muted)
        };
        let mark = if on { bar(th) } else { " " };
        let cursor = if on { "_" } else { "" };
        match fl.kind {
            Kind::Toggle => {
                let b = if fl.on { "[x]" } else { "[ ]" };
                lines.push(Line::from(vec![
                    Span::styled(mark, Style::new().fg(th.accent)),
                    Span::styled(format!("{b} {}", show(&fl.label)), label_style),
                    Span::styled(hint(fl), Style::new().fg(th.muted)),
                ]));
            }
            Kind::Pick => lines.push(Line::from(vec![
                Span::styled(mark, Style::new().fg(th.accent)),
                Span::styled(format!("{}: ", show(&fl.label)), label_style),
                Span::styled(hint(fl), Style::new().fg(th.muted)),
                Span::raw(if fl.hint.is_empty() { "" } else { " " }),
                Span::styled(
                    format!(
                        "< {} >",
                        show(fl.options.get(fl.idx).map_or("", String::as_str))
                    ),
                    Style::new().bold(),
                ),
            ])),
            Kind::Line | Kind::Text => {
                lines.push(Line::from(vec![
                    Span::styled(mark, Style::new().fg(th.accent)),
                    Span::styled(show(&fl.label), label_style),
                    Span::styled(hint(fl), Style::new().fg(th.muted)),
                ]));
                let text = show(&format!("{}{cursor}", fl.text));
                let rows: Vec<String> = wrap(&text, inner_w);
                let keep = if fl.kind == Kind::Text { 6 } else { 1 };
                let from = rows.len().saturating_sub(keep);
                for r in &rows[from..] {
                    lines.push(Line::from(vec![
                        Span::styled(mark, Style::new().fg(th.accent)),
                        Span::raw(format!(" {r}")),
                    ]));
                }
            }
        }
    }
    if let Some(e) = &form.error {
        lines.push(Line::raw(""));
        for r in wrap(&show(e), inner_w) {
            lines.push(Line::styled(
                format!(" {} {r}", th.ic.fail),
                Style::new().fg(th.err),
            ));
        }
    }
    let max_h = f.area().height.saturating_sub(2) as usize;
    let h = (lines.len() + 3).min(max_h).max(5) as u16;
    let area = popup(f, app, w, h, &form.title);
    let body_h = area.height.saturating_sub(1) as usize;
    let scroll = (focus_at + 1).saturating_sub(body_h);
    let [body, foot] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    f.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), body);
    let (hint, style) = if form.discarding {
        ("Discard this form? y/n", Style::new().fg(th.err).bold())
    } else {
        let long =
            "Tab/Shift-Tab field  Space toggle  Left/Right pick  Ctrl-S review command  Esc cancel";
        let hint = [
            long,
            "Tab field  Ctrl-S review command  Esc cancel",
            "Tab next  ^S review  Esc cancel",
        ]
        .into_iter()
        .find(|h| h.len() <= foot.width as usize)
        .unwrap_or("^S review  Esc cancel");
        (hint, Style::new().fg(th.muted))
    };
    f.render_widget(Paragraph::new(Span::styled(hint, style)), foot);
}

#[cfg(test)]
mod tests {

    #[test]
    fn open_prs_fact_pluralises() {
        assert_eq!(open_prs_fact(1, false), "1 open PR");
        assert_eq!(open_prs_fact(0, false), "0 open PRs");
        assert_eq!(open_prs_fact(74, false), "74 open PRs");
        assert_eq!(open_prs_fact(1, true), "1+ open PRs");
    }
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

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn repo_rows_name_the_repo_once_and_pr_rows_keep_owner_repo_number() {
        let a = app(false, IconSet::Unicode);
        let repo = Item {
            kind: Kind::Repo,
            repo: "o/r".into(),
            state: "fav".into(),
            meta: "public · Go · 2h".into(),
            ..Default::default()
        };
        for show_repo in [true, false] {
            let s = text(&label(&a, &repo, show_repo, 60));
            assert_eq!(s.matches("o/r").count(), 1, "{s:?}");
            assert!(s.contains("public · Go · 2h"), "{s:?}");
        }
        let pr = Item {
            kind: Kind::Pr,
            repo: "o/r".into(),
            number: 7,
            title: "t".into(),
            state: "open".into(),
            ..Default::default()
        };
        assert!(text(&label(&a, &pr, true, 60)).starts_with("o/r#7 "));
        assert!(text(&label(&a, &pr, false, 60)).starts_with("#7 "));
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
        let commits = vec![crate::gh::CommitRow {
            sha: "abc1234def5678".into(),
            subject: "Add the notes file".into(),
            author: "octocat".into(),
            when: "2026-01-02T00:00:00Z".into(),
            adds: 3,
            dels: 1,
        }];
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
                        ..Default::default()
                    }),
                ),
                (Tab::Checks, Data::Checks(checks)),
                (Tab::Comments, Data::Comments(comments.into())),
                (Tab::Commits, Data::Commits(commits.clone())),
            ],
        );
        a.seed_commit_diffs(&commits, "diff --git a/c.rs b/c.rs\n--- a/c.rs\n+++ b/c.rs\n@@ -1 +1 @@\n-old commit line\n+new commit line\ndiff --git a/d.rs b/d.rs\n--- a/d.rs\n+++ b/d.rs\n@@ -1 +1 @@\n-x\n+second file of the commit\n");
        a
    }

    #[test]
    fn layout_in_both_themes_and_sizes() {
        for light in [false, true] {
            for (w, h) in [(120, 40), (80, 24), (50, 12)] {
                let s = render(w, h, light, IconSet::Unicode);
                for want in [
                    "[1] ",
                    "[2] Files",
                    "[3] Issues",
                    "[4] Actions",
                    "[5] ",
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
            assert!(s.contains("[2] Files (2)"), "{w}x{h}\n{s}");
            key(&mut a, KeyCode::Char('2')); // focus Files: diff of file 1 shown, full width
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
        key(&mut a, KeyCode::Char('2'));
        key(&mut a, KeyCode::Char('j'));
        let s = render_app(&a, 120, 40);
        assert!(s.contains("◆2"), "thread badge\n{s}");
        key(&mut a, KeyCode::Char('v'));
        assert!(render_app(&a, 120, 40).contains('✓'));
        key(&mut a, KeyCode::Char('f'));
        let s = render_app(&a, 160, 30);
        assert!(
            !s.contains("[2] Files") && s.contains("auto:split"),
            "zoomed:\n{s}"
        );
        key(&mut a, KeyCode::Esc);
        assert!(render_app(&a, 120, 40).contains("[2] Files"));
    }

    #[test]
    fn drill_in_and_back_keeps_cursor() {
        for (w, h) in [(80, 24), (120, 40)] {
            let mut a = seeded();
            key(&mut a, KeyCode::Enter); // PR list focus: drill in
            let s = render_app(&a, w, h);
            for want in [
                "› PR #7",
                "[1] Files",
                "[2] Commits",
                "[3] Checks",
                "[4] Comments",
            ] {
                assert!(s.contains(want), "{w}x{h} missing {want:?}\n{s}");
            }
            assert!(
                !s.contains("Mine"),
                "the PR list is parked while drilled in"
            );
            key(&mut a, KeyCode::Char(']')); // next panel: Commits
            assert!(render_app(&a, w, h).contains("Commit abc1234"));
            key(&mut a, KeyCode::Char(']')); // Checks
            assert!(render_app(&a, w, h).contains("Log: build"));
            key(&mut a, KeyCode::Char(']')); // Comments: thread text
            assert!(render_app(&a, w, h).contains("please rename"));
            key(&mut a, KeyCode::Esc);
            let s = render_app(&a, w, h);
            assert!(
                s.contains("[1] ") && s.contains("Mine") && !s.contains("› PR")
                    || s.contains("[1] Pull requests"),
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
        key(&mut a, KeyCode::Char('2')); // Files focus: last file, no toggle, and it says why
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
            "previous/next list tab",
            "Repo panel (Branches / Tags / Releases tabs)",
            "m merge",
            "f zoom",
            "g/G first/last row",
        ] {
            assert!(s.contains(want), "200 cols: missing {want:?}\n{s}");
        }
        let s = render_app(&a, 80, 24); // too small for everything: wraps and scrolls
        assert!(
            s.contains("Keys (j/k scroll") && s.contains("Panels (default"),
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
        key(&mut a, KeyCode::Char('2'));
        assert!(render_app(&a, 120, 40).contains("auto:unified")); // Diff by default
        key(&mut a, KeyCode::Char(']')); // Commits tab
        key(&mut a, KeyCode::Char(']')); // wraps to Overview
        let s = render_app(&a, 120, 40);
        assert!(s.contains("[open]") && !s.contains("auto:unified"), "{s}");
    }

    #[test]
    fn viewed_tick_keeps_stats_aligned() {
        let mut a = seeded();
        key(&mut a, KeyCode::Char('2'));
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
                !s.contains("[2] Files"),
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
        assert!(a.browser.is_none() && render_app(&a, 120, 40).contains("[2] Files"));
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
        a.seed(pr, vec![(Tab::Comments, Data::Comments(vec![e].into()))]);
        key(&mut a, KeyCode::Enter); // drill in
        key(&mut a, KeyCode::Char(']')); // Commits
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
        a.seed(pr, vec![(Tab::Comments, Data::Comments(vec![e].into()))]);
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
        a.seed(pr, vec![(Tab::Comments, Data::Comments(vec![e].into()))]);
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
        a.seed(pr, vec![(Tab::Comments, Data::Comments(vec![e].into()))]);
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

    /// The current hunk's header stays pinned under the file header while its body scrolls.
    #[test]
    fn sticky_hunk_bar_pins_the_current_hunk() {
        let mut d = String::from(
            "diff --git a/m.txt b/m.txt\n--- a/m.txt\n+++ b/m.txt\n@@ -1,40 +1,40 @@ first hunk\n",
        );
        for i in 1..=40 {
            d += &format!(" first body line {i}\n");
        }
        d += "@@ -100,40 +100,40 @@ second hunk\n";
        for i in 1..=40 {
            d += &format!(" second body line {i}\n");
        }
        for width in [80u16, 220] {
            let mut a = seeded();
            let pr = a.pr_item().cloned().unwrap();
            let files = crate::diff::parse(&d);
            a.seed(
                pr,
                vec![(
                    Tab::Diff,
                    Data::Files(FilesData {
                        sha: "x".into(),
                        files,
                        threads: Default::default(),
                        ..Default::default()
                    }),
                )],
            );
            key(&mut a, KeyCode::Char('2'));
            key(&mut a, KeyCode::Char('l'));
            key(&mut a, KeyCode::Char('t')); // auto -> unified
            if width == 220 {
                key(&mut a, KeyCode::Char('t')); // unified -> split
            }
            let body_rows = |s: &str| -> Vec<String> {
                s.lines()
                    .skip(3)
                    .map(|l| {
                        l.chars()
                            .skip(if width == 80 { 31 } else { 91 })
                            .collect::<String>()
                    })
                    .collect()
            };
            let s = render_app(&a, width, 24);
            let count = |s: &str, needle: &str| s.matches(needle).count();
            assert_eq!(
                count(&s, "first hunk"),
                1,
                "not pinned while its own header is on screen\n{s}"
            );
            key(&mut a, KeyCode::Char('G')); // cursor and view to the end of the file
            render_app(&a, width, 24);
            let s = render_app(&a, width, 24);
            let rows = body_rows(&s);
            assert!(
                rows[0].contains("second hunk"),
                "{width}: pinned bar is the first body row\n{s}"
            );
            assert_eq!(count(&s, "second hunk"), 1, "{width}: not duplicated\n{s}");
            assert!(!s.contains("first hunk"), "{s}");
            assert!(rows.iter().any(|r| r.contains("second body line 40")));
        }
    }

    #[test]
    fn commits_panel_lists_commits_and_shows_the_selected_commits_diff() {
        for (w, h) in [(80, 24), (120, 40)] {
            let mut a = seeded();
            key(&mut a, KeyCode::Enter); // drill in
            let s = render_app(&a, w, h);
            assert!(s.contains("[2] Commits (1)"), "{w}x{h}\n{s}");
            if w >= 100 {
                // (short terminals collapse unfocused panels to their title)
                for want in ["abc1234", "Add the notes", "+3 -1"] {
                    assert!(s.contains(want), "{w}x{h} missing {want:?}\n{s}");
                }
            }
            key(&mut a, KeyCode::Char('2')); // focus Commits: its diff, not the PR's
            let s = render_app(&a, w, h);
            for want in [
                "Commit abc1234 Add the notes file",
                "c.rs",
                "1/2",
                "new commit line",
                "auto:",
            ] {
                assert!(s.contains(want), "{w}x{h} missing {want:?}\n{s}");
            }
            assert!(
                !s.contains("viewed"),
                "viewed marks are for PR files only\n{s}"
            );
            key(&mut a, KeyCode::Char('n')); // next file of the commit
            let s = render_app(&a, w, h);
            assert!(
                s.contains("d.rs") && s.contains("second file of the commit") && s.contains("2/2"),
                "{s}"
            );
            key(&mut a, KeyCode::Char('l')); // Enter/l focuses the diff; j moves its line cursor
            key(&mut a, KeyCode::Char('j'));
            assert!(a.row > 0, "row cursor moves in the commit diff");
            key(&mut a, KeyCode::Char('t'));
            key(&mut a, KeyCode::Char('w'));
            assert!(render_app(&a, w, h).contains("clip"));
        }
    }

    #[test]
    fn pr_commits_tab_lists_commits() {
        let mut a = seeded();
        key(&mut a, KeyCode::Char(']')); // Checks
        key(&mut a, KeyCode::Char(']')); // Comments
        key(&mut a, KeyCode::Char(']')); // Diff
        key(&mut a, KeyCode::Char(']')); // Commits
        let s = render_app(&a, 120, 30);
        assert!(
            s.contains("abc1234 Add the notes file")
                && s.contains("octocat")
                && s.contains("+3 -1"),
            "{s}"
        );
    }

    #[test]
    fn paged_comments_show_progress_and_a_more_row() {
        let mut cd =
            crate::gh::parse_comments(include_str!("../tests/comments_p1.json"), true).unwrap();
        cd.loading = true;
        let mut a = seeded();
        let pr = a.pr_item().cloned().unwrap();
        a.seed(pr.clone(), vec![(Tab::Comments, Data::Comments(cd))]);
        key(&mut a, KeyCode::Char(']'));
        key(&mut a, KeyCode::Char(']')); // Comments tab
        let s = render_app(&a, 120, 40);
        assert!(
            s.contains("loading more\u{2026} (6/10)") && s.contains("4 more"),
            "{s}"
        );
        // drill-in list: count says "n+" and the last row is the "N more" placeholder
        key(&mut a, KeyCode::Enter);
        key(&mut a, KeyCode::Char('4')); // Comments panel
        let s = render_app(&a, 120, 40);
        assert!(
            s.contains("[4] Comments (4+)") && s.contains("4 more (loading)"),
            "{s}"
        );
        // a failed page says why and how to retry
        let mut cd =
            crate::gh::parse_comments(include_str!("../tests/comments_p1.json"), true).unwrap();
        cd.apply(crate::gh::More::Failed(
            "rate limited".into(),
            crate::gh::Pending::default(),
        ));
        let mut a = seeded();
        a.seed(pr, vec![(Tab::Comments, Data::Comments(cd))]);
        key(&mut a, KeyCode::Char(']'));
        key(&mut a, KeyCode::Char(']'));
        let s = render_app(&a, 120, 40);
        assert!(
            s.contains("4 more not loaded: rate limited") && s.contains("m to retry"),
            "{s}"
        );
    }

    #[test]
    fn paused_and_rate_limited_comment_paging_say_so() {
        let first =
            || crate::gh::parse_comments(include_str!("../tests/comments_p1.json"), true).unwrap();
        // automatic budget used up: nothing loading, cursors kept -> an on-demand hint
        let mut cd = first();
        let rest = cd.pend.clone();
        cd.apply(crate::gh::More::Paused(rest));
        assert!(cd.paused());
        let mut a = seeded();
        let pr = a.pr_item().cloned().unwrap();
        a.seed(pr.clone(), vec![(Tab::Comments, Data::Comments(cd))]);
        key(&mut a, KeyCode::Char(']'));
        key(&mut a, KeyCode::Char(']'));
        let s = render_app(&a, 120, 40);
        assert!(
            s.contains("4 more not loaded yet (6/10): scroll down or press m"),
            "{s}"
        );
        // a rate limit says when to retry, not "repo not found", and `m` refuses until then
        let mut cd = first();
        let rest = cd.pend.clone();
        cd.apply(crate::gh::More::RateLimited(90, rest));
        let mut a = seeded();
        a.seed(pr, vec![(Tab::Comments, Data::Comments(cd))]);
        key(&mut a, KeyCode::Char(']'));
        key(&mut a, KeyCode::Char(']'));
        let s = render_app(&a, 120, 40);
        assert!(
            s.contains("GitHub rate limit \u{2014} retry in 9") && !s.contains("not found"),
            "{s}"
        );
        key(&mut a, KeyCode::Char('m')); // would normally open the merge menu; here it is "load more"
        assert!(
            a.modal.is_none() && a.status.contains("GitHub rate limit \u{2014} retry in"),
            "{}",
            a.status
        );
    }

    /// Regression (ghost text in the detail pane): emoji with a variation selector ("\u{26a0}\u{fe0f}") are 2
    /// cells to ratatui but 1 to many terminals, which shifted later cells and left leftovers of the
    /// previous PR's text. Selectors are dropped on the way in, and a frame drawn over a longer one
    /// must equal the same frame drawn fresh.
    #[test]
    fn no_ghost_cells_after_a_longer_frame_and_no_variation_selectors() {
        let json = r#"[{"number":1,"title":"A","url":"u","state":"open","body":"\u26a0\ufe0f   Heads up \u26a0\ufe0f","author":{"login":"a"},"labels":[{"name":"dependencies"},{"name":"github_actions"}]},
                        {"number":2,"title":"B","url":"u","state":"open","body":"short","author":{"login":"a"},"labels":[{"name":"javascript"}]}]"#;
        let items = crate::gh::parse_items(json, Kind::Pr, "o/r").unwrap();
        let mut a = app(false, IconSet::Unicode);
        let i = a.panel_idx_for_test(crate::app::PK::Prs);
        a.panels[i].items = items;
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let dump = |t: &Terminal<TestBackend>| -> Vec<String> {
            let b = t.backend().buffer();
            (0..30)
                .map(|y| {
                    (0..120)
                        .map(|x| b[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect()
        };
        t.draw(|f| draw(f, &a)).unwrap();
        let first = dump(&t);
        assert!(
            first.iter().all(|r| !r.contains('\u{fe0f}')),
            "no variation selectors on screen"
        );
        assert!(
            first.iter().any(|r| r.contains("github_actions"))
                && first.iter().any(|r| r.contains("\u{26a0} "))
        );
        key(&mut a, KeyCode::Char('j')); // the shorter PR
        t.draw(|f| draw(f, &a)).unwrap();
        let over_old = dump(&t);
        let fresh = render_app(&a, 120, 30);
        assert_eq!(
            over_old.join("\n"),
            fresh,
            "stale cells left over from the previous frame"
        );
        assert!(!fresh.contains("github_actions") && !fresh.contains("Heads up"));
    }

    #[test]
    fn files_panel_shows_the_exact_count() {
        let mut a = seeded();
        let pr = a.pr_item().cloned().unwrap();
        let diff: String = (0..252).map(|i| format!("diff --git a/f{i}.txt b/f{i}.txt\n--- a/f{i}.txt\n+++ b/f{i}.txt\n@@ -1 +1 @@\n-a\n+b\n")).collect();
        let files = crate::diff::parse(&diff);
        a.seed(
            pr,
            vec![(
                Tab::Diff,
                Data::Files(FilesData {
                    sha: "x".into(),
                    files,
                    threads: Default::default(),
                    ..Default::default()
                }),
            )],
        );
        let s = render_app(&a, 120, 40);
        assert!(s.contains("[2] Files (252)") && !s.contains("252+"), "{s}");
    }

    fn type_str(a: &mut App, s: &str) {
        for c in s.chars() {
            key(a, KeyCode::Char(c));
        }
    }

    fn ctrl_s(a: &mut App) {
        a.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    }

    fn form_mut(a: &mut App) -> &mut crate::form::Form {
        match &mut a.modal {
            Some(Modal::Form(f)) => f,
            _ => panic!("no form open"),
        }
    }

    #[test]
    fn new_issue_form_validates_then_confirms_the_exact_command() {
        for (w, h) in [(80, 24), (120, 40)] {
            let mut a = seeded();
            key(&mut a, KeyCode::Char('3')); // Issues panel
            key(&mut a, KeyCode::Char('n'));
            let s = render_app(&a, w, h);
            for want in [
                "New issue",
                "Title",
                "Body",
                "Labels",
                "Assignees",
                "Ctrl-S review",
            ] {
                assert!(s.contains(want), "{w}x{h} missing {want:?}\n{s}");
            }
            ctrl_s(&mut a); // empty title
            assert!(
                render_app(&a, w, h).contains("Title is required"),
                "{w}x{h}"
            );
            form_mut(&mut a).set_labels(vec!["bug".into(), "docs".into()]);
            type_str(&mut a, "Crash on start");
            key(&mut a, KeyCode::Tab); // body
            type_str(&mut a, "steps");
            key(&mut a, KeyCode::Tab); // labels
            type_str(&mut a, "nope");
            ctrl_s(&mut a);
            let s = render_app(&a, w, h);
            assert!(
                s.contains("Unknown label 'nope'") && s.contains("bug, docs"),
                "{w}x{h}\n{s}"
            );
            for _ in 0..4 {
                key(&mut a, KeyCode::Backspace);
            }
            type_str(&mut a, "BUG");
            ctrl_s(&mut a);
            let s = render_app(&a, w, h);
            assert!(
                s.contains("Run this command?")
                    && s.contains(
                        "gh issue create -R o/r --title 'Crash on start' --body steps --label bug"
                    ),
                "{w}x{h}\n{s}"
            );
            key(&mut a, KeyCode::Enter); // Enter must not run it
            assert!(render_app(&a, w, h).contains("Run this command?"));
            key(&mut a, KeyCode::Esc);
            assert!(
                render_app(&a, w, h).contains("New issue") && !a.status.contains("running"),
                "cancelling the confirm goes back to the form, nothing ran"
            );
            key(&mut a, KeyCode::Esc);
            assert!(render_app(&a, w, h).contains("Discard this form? y/n"));
            key(&mut a, KeyCode::Char('y'));
            assert!(a.modal.is_none());
        }
    }

    #[test]
    fn new_pr_form_from_a_branch_warns_when_it_is_not_pushed() {
        let mut a = seeded();
        let i = a.panel_idx_for_test(crate::app::PK::Repo);
        a.panels[i].items = vec![Item {
            title: "feat/x".into(),
            cmd: "feat/x".into(),
            repo: "o/r".into(),
            kind: Kind::Branch,
            ..Default::default()
        }];
        key(&mut a, KeyCode::Char('5')); // Repo: Branches tab
        key(&mut a, KeyCode::Char('x'));
        let menu = render_app(&a, 120, 40);
        assert!(menu.contains("Create pull request..."), "{menu}");
        key(&mut a, KeyCode::Char('j'));
        key(&mut a, KeyCode::Enter);
        let s = render_app(&a, 120, 40);
        assert!(
            s.contains("New pull request from feat/x")
                && s.contains("Base branch")
                && s.contains("Open as draft"),
            "{s}"
        );
        form_mut(&mut a).set_pr_data("main", vec!["dev".into()], Some(false), None);
        type_str(&mut a, "Add x");
        let s = render_app(&a, 120, 40);
        assert!(
            s.contains("not found on o/r") && s.contains("git push -u origin feat/x"),
            "pushing is the user's job\n{s}"
        );
        ctrl_s(&mut a);
        let s = render_app(&a, 120, 40);
        assert!(s.contains("gh pr create"), "a warning, not a block\n{s}");
        key(&mut a, KeyCode::Char('n'));
        let s = render_app(&a, 120, 40);
        assert!(
            s.contains("New pull request from feat/x") && s.contains("Add x"),
            "cancelling the confirm reopens the form with its text\n{s}"
        );
        form_mut(&mut a).set_pr_data(
            "main",
            vec!["dev".into()],
            Some(true),
            Some("## Summary\n".into()),
        );
        ctrl_s(&mut a);
        let s = render_app(&a, 120, 40);
        assert!(
            s.contains("gh pr create -R o/r --head feat/x --base main --title 'Add x' --body"),
            "{s}"
        );
    }

    #[test]
    fn new_pr_needs_a_local_clone_for_the_current_branch() {
        let mut a = seeded(); // PR panel focus, cwd is not a clone in tests
        key(&mut a, KeyCode::Char('n'));
        assert!(
            a.modal.is_none() && a.status.contains("not a clone"),
            "{}",
            a.status
        );
    }

    #[test]
    fn run_workflow_form_from_the_workflows_tab() {
        for (w, h) in [(80, 24), (120, 40)] {
            let mut a = seeded();
            let i = a.panel_idx_for_test(crate::app::PK::Actions);
            a.panels[i].tab = 1;
            let wf = |n: u64, name: &str, state: &str| Item {
                number: n,
                title: name.into(),
                state: state.into(),
                cmd: format!(".github/workflows/{name}.yml"),
                repo: "o/r".into(),
                kind: Kind::Workflow,
                ..Default::default()
            };
            a.panels[i].items = vec![
                wf(4242, "Deploy", "active"),
                wf(7, "Old", "disabled_manually"),
            ];
            key(&mut a, KeyCode::Char('4')); // Actions
            key(&mut a, KeyCode::Char('d'));
            let s = render_app(&a, w, h);
            assert!(
                s.contains("Run workflow: Deploy") && s.contains("Branch or tag to run on"),
                "{w}x{h}\n{s}"
            );
            form_mut(&mut a).set_dispatch(
                "main",
                vec!["dev".into()],
                vec![],
                Some(crate::dispatch::parse(include_str!(
                    "../tests/workflow_inputs.yml"
                ))),
            );
            let s = render_app(&a, 120, 60);
            for want in [
                "message",
                "(required) What to say",
                "[x] verbose",
                "level",
                "< warn >",
                "count",
                "target",
                "plain",
            ] {
                assert!(s.contains(want), "missing {want:?}\n{s}");
            }
            ctrl_s(&mut a);
            assert!(
                render_app(&a, w, h).contains("'message' is required"),
                "{w}x{h}"
            );
            key(&mut a, KeyCode::Tab); // first input
            type_str(&mut a, "ship it");
            ctrl_s(&mut a);
            let s = render_app(&a, w, h);
            // (the box wraps the command at 80 columns: compare with borders and line breaks removed)
            // (panel rows behind the popup's edges leave stray cells: keep each row's widest segment)
            let flat: String = s
                .lines()
                .filter_map(|l| l.split('\u{2502}').max_by_key(|p| p.chars().count()))
                .collect::<Vec<_>>()
                .join(" ");
            let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                flat.contains("gh workflow run 4242 -R o/r --ref main -f 'message=ship it' -f verbose=true -f level=warn -f count=3"),
                "{w}x{h}\n{s}"
            );
            key(&mut a, KeyCode::Esc);
            assert!(render_app(&a, w, h).contains("Run workflow: Deploy"));
            key(&mut a, KeyCode::Esc);
            if a.modal.is_some() {
                key(&mut a, KeyCode::Char('y'));
            }
            assert!(a.modal.is_none());
            // a disabled workflow offers no run
            key(&mut a, KeyCode::Char('j'));
            key(&mut a, KeyCode::Char('d'));
            assert!(a.modal.is_none());
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
                    ..Default::default()
                }),
            )],
        );
        key(&mut a, KeyCode::Char('2'));
        key(&mut a, KeyCode::Char('l'));
        for _ in 0..12 {
            key(&mut a, KeyCode::Char('G')); // cursor to the last row: worst case for offsets
        }
        for (w, mode) in [(120u16, "unified/wrap"), (220, "split/wrap")] {
            // the first frame also pays the one-time syntax highlighting of everything up to the cursor
            let t = std::time::Instant::now();
            render_app(&a, w, 40);
            let cold = t.elapsed();
            let t = std::time::Instant::now();
            for _ in 0..10 {
                render_app(&a, w, 40);
            }
            println!(
                "{mode}: cold first frame {cold:?}, then {:?} per frame",
                t.elapsed() / 10
            );
        }
    }

    #[test]
    fn form_neutralizes_yaml_names_and_fits_the_minimum_size() {
        let mut a = seeded();
        let mut f = crate::form::Form::dispatch("o/r", "1", "CI");
        let y = "on:\n  workflow_dispatch:\n    inputs:\n      \"ev\u{202e}il\":\n        required: true\n";
        f.set_dispatch("main", vec![], vec![], Some(crate::dispatch::parse(y)));
        a.modal = Some(crate::app::Modal::Form(Box::new(f)));
        let s = render_app(&a, 50, 14);
        assert!(s.contains("ev<U+202E>il") && !s.contains('\u{202e}'), "{s}");
        assert!(
            s.contains("Esc cancel"),
            "footer fits the minimum width\n{s}"
        );
        ctrl_s(&mut a);
        let s = render_app(&a, 50, 14);
        assert!(
            !s.contains('\u{202e}'),
            "error text is neutralized too\n{s}"
        );
    }

    fn long_confirm() -> App {
        let mut a = seeded();
        let mut c = crate::app::Confirm::new_for_test(
            [
                "gh",
                "pr",
                "create",
                "--body",
                &"line of the body\n".repeat(60),
                "--reviewer",
                "rev1",
            ]
            .map(String::from)
            .to_vec(),
        );
        c.scroll = 0;
        a.modal = Some(crate::app::Modal::Confirm(c));
        a
    }

    #[test]
    fn long_confirm_scrolls_to_the_end_and_refuses_y_until_then() {
        for (w, h) in [(80u16, 24u16), (120, 30)] {
            let mut a = long_confirm();
            let s = render_app(&a, w, h);
            assert!(
                s.contains("more (command continues") && !s.contains("--reviewer rev1"),
                "{w}x{h}\n{s}"
            );
            key(&mut a, KeyCode::Char('y'));
            assert!(
                a.modal.is_some() && a.status.contains("scroll to the end"),
                "y refused"
            );
            key(&mut a, KeyCode::PageDown);
            assert!(
                render_app(&a, w, h).contains("more"),
                "PgDn moves a page, not to the end"
            );
            key(&mut a, KeyCode::Char('G'));
            let s = render_app(&a, w, h);
            assert!(
                s.contains("--reviewer rev1") && s.contains("end of command"),
                "{w}x{h}\n{s}"
            );
            key(&mut a, KeyCode::Home);
            assert!(render_app(&a, w, h).contains("more"));
            key(&mut a, KeyCode::End);
            render_app(&a, w, h);
            key(&mut a, KeyCode::Esc);
            assert!(a.modal.is_none());
        }
    }

    #[test]
    fn ascii_focus_bar_and_choice_spacing() {
        let mut a = App::with("o/r".into(), Theme::new(false, IconSet::Ascii, true), false);
        let mut f = crate::form::Form::dispatch("o/r", "1", "CI");
        let y = "on:\n  workflow_dispatch:\n    inputs:\n      test:\n        description: which\n        type: choice\n        options: [all, fast]\n";
        f.set_dispatch("main", vec![], vec![], Some(crate::dispatch::parse(y)));
        a.modal = Some(crate::app::Modal::Form(Box::new(f)));
        let s = render_app(&a, 100, 20);
        assert!(!s.contains('\u{258e}') && s.contains('|'), "{s}");
        assert!(s.contains("test:   which < all >"), "{s}");
    }

    fn cfg_app(toml: &str) -> App {
        let cfg = crate::config::parse(toml, "t").unwrap();
        App::build(
            "o/r".into(),
            Theme::new(false, IconSet::Unicode, true),
            false,
            cfg,
        )
    }

    fn items(kind: Kind, n: usize) -> Vec<Item> {
        (1..=n)
            .map(|i| Item {
                number: i as u64,
                title: format!("item number {i}"),
                repo: "o/r".into(),
                state: "open".into(),
                kind,
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn default_layout_is_five_panels_with_room_for_rows() {
        for (w, h) in [(80u16, 24u16), (120, 40)] {
            let mut a = app(false, IconSet::Unicode);
            let i = a.panel_idx_for_test(crate::app::PK::Prs);
            a.panels[i].items = items(Kind::Pr, 8);
            let s = render_app(&a, w, h);
            for want in ["[1] ", "[2] Files", "[3] Issues", "[4] Actions", "[5] "] {
                assert!(s.contains(want), "{w}x{h} missing {want:?}\n{s}");
            }
            for gone in ["Status", "Notifications", "[6]"] {
                assert!(!s.contains(gone), "{w}x{h} still has {gone:?}\n{s}");
            }
            if h == 24 {
                assert!(
                    s.contains("#8 item number 8"),
                    "eight rows fit at 80x24\n{s}"
                );
            }
        }
    }

    #[test]
    fn every_tab_shows_its_count_and_dots_until_known() {
        let mut a = app(false, IconSet::Unicode);
        let i = a.panel_idx_for_test(crate::app::PK::Repo);
        a.panels[i].items = items(Kind::Branch, 32);
        a.counts.insert((crate::app::PK::Repo, 1), (14, false));
        let s = render_app(&a, 120, 40);
        assert!(
            s.contains("[5] Branches 32 \u{b7} Tags 14 \u{b7} Releases \u{2026}"),
            "{s}"
        );
        a.counts.insert((crate::app::PK::Repo, 2), (100, true));
        let s = render_app(&a, 120, 40);
        assert!(s.contains("Releases 100+"), "{s}");
        // too narrow for every label: short names, then just the showing tab
        let s = render_app(&a, 80, 24);
        assert!(
            s.contains("[5] Branches (32)") || s.contains("[5] Brn 32"),
            "{s}"
        );
        let s = render_app(&app(false, IconSet::Ascii), 120, 40);
        assert!(s.contains("Releases ~"), "ascii uses ~ for unknown\n{s}");
    }

    #[test]
    fn the_active_tab_is_highlighted_and_clicking_a_label_switches() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut a = app(false, IconSet::Unicode);
        render_app(&a, 120, 40);
        let t = *a
            .hit
            .borrow()
            .ptabs
            .iter()
            .find(|t| t.panel == 4 && t.tab == 1)
            .expect("Tags label is clickable");
        a.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: t.x0 + 1,
            row: t.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(
            (a.focus, a.panels[4].tab),
            (4, 1),
            "focus follows, tab switched"
        );
        // the showing tab is underlined, the others are not
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw(f, &a)).unwrap();
        let b = term.backend().buffer();
        let row = a
            .hit
            .borrow()
            .ptabs
            .iter()
            .find(|t| t.panel == 4)
            .map(|t| t.y)
            .unwrap();
        let ul = |x: u16| b[(x, row)].modifier.contains(Modifier::UNDERLINED);
        let (tags, branches) = (
            *a.hit
                .borrow()
                .ptabs
                .iter()
                .find(|t| t.panel == 4 && t.tab == 1)
                .unwrap(),
            *a.hit
                .borrow()
                .ptabs
                .iter()
                .find(|t| t.panel == 4 && t.tab == 0)
                .unwrap(),
        );
        assert!(ul(tags.x0) && !ul(branches.x0));
    }

    #[test]
    fn number_key_again_cycles_the_list_tab_and_braces_step() {
        let mut a = app(false, IconSet::Unicode);
        assert_eq!(a.panels[0].tab, 0);
        key(&mut a, KeyCode::Char('1'));
        assert_eq!(a.panels[0].tab, 1, "focused panel's number: next tab");
        for _ in 0..3 {
            key(&mut a, KeyCode::Char('1'));
        }
        assert_eq!(a.panels[0].tab, 0, "wraps");
        key(&mut a, KeyCode::Char('3'));
        assert_eq!(
            (a.focus, a.panels[2].tab),
            (2, 0),
            "another panel's number only focuses"
        );
        key(&mut a, KeyCode::Char('}'));
        key(&mut a, KeyCode::Char('{'));
        key(&mut a, KeyCode::Char('{'));
        assert_eq!(a.panels[2].tab, 2, "braces still step, backwards wraps");
    }

    #[test]
    fn custom_three_panel_config_numbers_by_order_and_hides_files_without_prs() {
        for (w, h) in [(80u16, 24u16), (120, 40)] {
            let mut a = cfg_app(
                "[panels]\nshow = [\"repo\", \"issues\", \"prs\"]\n[panels.repo]\ntabs = [\"tags\", \"branches\"]\ndefault_tab = \"branches\"\n",
            );
            let s = render_app(&a, w, h);
            for want in ["[1] ", "[2] Issues", "[3] "] {
                assert!(s.contains(want), "{w}x{h} missing {want:?}\n{s}");
            }
            for gone in ["Files", "Actions", "[4]", "Releases"] {
                assert!(!s.contains(gone), "{w}x{h} has {gone:?}\n{s}");
            }
            assert_eq!(a.panels[0].tab, 1, "default_tab = branches");
            key(&mut a, KeyCode::Char('3'));
            assert_eq!(a.panels[a.focus].kind, crate::app::PK::Prs);
            key(&mut a, KeyCode::Char('4'));
            assert_eq!(a.focus, 2, "no fourth panel");
        }
    }

    #[test]
    fn diff_file_navigation_works_without_a_files_panel() {
        let mut a = cfg_app("[panels]\nshow = [\"prs\"]\n");
        let diff = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\ndiff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-x\n+y\n";
        let pr = items(Kind::Pr, 1).remove(0);
        a.seed(
            pr,
            vec![(
                Tab::Diff,
                Data::Files(FilesData {
                    sha: "s".into(),
                    files: crate::diff::parse(diff),
                    ..Default::default()
                }),
            )],
        );
        for _ in 0..3 {
            key(&mut a, KeyCode::Char(']')); // Overview .. Diff
        }
        key(&mut a, KeyCode::Char('l'));
        key(&mut a, KeyCode::Char('n'));
        assert_eq!(a.file(), 1);
        assert!(render_app(&a, 120, 40).contains("2/2"));
    }

    #[test]
    fn hide_empty_collapses_empty_panels_to_a_line() {
        let mut a = cfg_app("[panels]\nhide_empty = true\n");
        let i = a.panel_idx_for_test(crate::app::PK::Prs);
        a.panels[i].items = items(Kind::Pr, 2);
        for (k, tabs) in [
            (crate::app::PK::Issues, 3),
            (crate::app::PK::Actions, 2),
            (crate::app::PK::Repo, 3),
        ] {
            for t in 1..tabs {
                a.counts.insert((k, t), (0, false));
            }
        }
        let s = render_app(&a, 120, 40);
        assert_eq!(
            s.matches("(empty)").count(),
            4,
            "files, issues, actions, repo\n{s}"
        );
        // a panel with something in another tab stays open; so does the focused one
        a.counts.insert((crate::app::PK::Issues, 2), (5, false));
        let s = render_app(&a, 120, 40);
        assert_eq!(s.matches("(empty)").count(), 3, "{s}");
        key(&mut a, KeyCode::Char('4'));
        let s = render_app(&a, 120, 40);
        assert_eq!(
            s.matches("(empty)").count(),
            2,
            "focus expands a panel\n{s}"
        );
        let s = render_app(&app(false, IconSet::Unicode), 120, 40);
        assert!(!s.contains("(empty)"), "off by default");
    }

    #[test]
    fn header_shows_repo_facts_and_the_unread_badge() {
        let mut a = app(false, IconSet::Unicode);
        a.meta = Some(crate::gh::RepoMeta {
            stars: 46514,
            private: false,
            branch: "trunk".into(),
            ..Default::default()
        });
        a.counts.insert((crate::app::PK::Prs, 2), (74, false));
        a.seed_unread(&["a/b", "c/d", "c/d"]);
        let s = render_app(&a, 120, 40);
        let head = s.lines().next().unwrap();
        assert!(
            head.contains("\u{2605} 46514 \u{b7} public \u{b7} default trunk \u{b7} 74 open PRs"),
            "{head}"
        );
        assert!(
            head.trim_end().ends_with("\u{2709} 3"),
            "badge keeps the right edge: {head}"
        );
        a.cfg.repos.toggle_hidden("c/d");
        assert!(
            render_app(&a, 120, 40)
                .lines()
                .next()
                .unwrap()
                .trim_end()
                .ends_with("\u{2709} 1"),
            "hidden repos don't count"
        );
        a.seed_unread(&[]);
        assert!(!render_app(&a, 120, 40).contains('\u{2709}'), "hidden at 0");
    }

    fn notif(kind: Kind, repo: &str, n: u64, title: &str, id: &str) -> Item {
        Item {
            kind,
            repo: repo.into(),
            number: n,
            title: title.into(),
            state: "unread".into(),
            meta: format!("{repo} \u{b7} mention"),
            cmd: id.into(),
            url: format!("https://github.com/{repo}/pull/{n}"),
            ..Default::default()
        }
    }

    #[test]
    fn inbox_lists_notifications_and_marking_read_confirms_the_exact_command() {
        for (w, h) in [(80u16, 24u16), (120, 40)] {
            let mut a = app(false, IconSet::Unicode);
            a.seed_inbox(vec![
                notif(Kind::Pr, "o/r", 7, "Add notes", "1001"),
                notif(Kind::Issue, "x/y", 9, "Crash on start", "1002"),
                notif(Kind::Other, "x/y", 0, "Release v2", "1003"),
            ]);
            let s = render_app(&a, w, h);
            for want in [
                "Inbox  3 unread",
                "o/r",
                "#7 Add notes",
                "x/y",
                "Crash on start",
                "mention",
                "m mark read",
                "Esc back",
            ] {
                assert!(s.contains(want), "{w}x{h} missing {want:?}\n{s}");
            }
            assert!(!s.contains("[1] "), "full screen, no panels\n{s}");
            key(&mut a, KeyCode::Char('j'));
            key(&mut a, KeyCode::Char('m'));
            let s = render_app(&a, w, h);
            assert!(
                s.contains("Run this command?")
                    && s.contains("gh api -X PATCH notifications/threads/1002"),
                "{w}x{h}\n{s}"
            );
            key(&mut a, KeyCode::Enter); // never runs
            assert!(render_app(&a, w, h).contains("Run this command?"));
            key(&mut a, KeyCode::Char('n'));
            assert!(
                a.modal.is_none() && a.inbox.is_some(),
                "cancel returns to the inbox"
            );
            key(&mut a, KeyCode::Esc);
            assert!(a.inbox.is_none() && render_app(&a, w, h).contains("[1] "));
        }
    }

    #[test]
    fn inbox_states_and_hidden_repos() {
        let mut a = app(false, IconSet::Unicode);
        key(&mut a, KeyCode::Char('N'));
        assert!(
            render_app(&a, 120, 40).contains("loading"),
            "loading until the fetch answers"
        );
        a.seed_inbox(vec![]);
        assert!(render_app(&a, 120, 40).contains("no unread notifications"));
        key(&mut a, KeyCode::Char('N'));
        assert!(a.inbox.is_none(), "the same key closes it");
    }

    #[test]
    fn tags_list_and_checkout_is_not_offered() {
        let mut a = app(false, IconSet::Unicode);
        let i = a.panel_idx_for_test(crate::app::PK::Repo);
        a.panels[i].tab = 1;
        a.panels[i].items = vec![Item {
            title: "v1.2.0".into(),
            cmd: "v1.2.0".into(),
            meta: "abc1234".into(),
            repo: "o/r".into(),
            kind: Kind::Tag,
            ..Default::default()
        }];
        key(&mut a, KeyCode::Char('5'));
        let s = render_app(&a, 120, 40);
        assert!(
            s.contains("v1.2.0  abc1234") && s.contains("Branches"),
            "{s}"
        );
        key(&mut a, KeyCode::Char('c'));
        assert!(a.status.contains("not available for tags"), "{}", a.status);
    }

    fn line_of(s: &str, needle: &str) -> String {
        s.lines()
            .find(|l| l.contains(needle))
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn header_drops_facts_from_the_right_and_keeps_the_badge() {
        let mut a = app(false, IconSet::Unicode);
        a.header = " cli/cli  branch: feat  user: octocat".into();
        a.meta = Some(crate::gh::RepoMeta {
            stars: 46514,
            branch: "trunk".into(),
            ..Default::default()
        });
        a.counts.insert((crate::app::PK::Prs, 2), (74, false));
        a.seed_unread(&["a/b"]);
        let facts = ["\u{2605} 46514", "public", "default trunk", "74 open PRs"];
        let mut last = facts.len();
        for w in [120u16, 80, 70, 60, 50] {
            let s = render_app(&a, w, 24);
            let head = s.lines().next().unwrap().trim_end().to_string();
            assert!(
                head.ends_with("\u{2709} 1"),
                "{w}: badge keeps the edge: {head:?}"
            );
            assert!(
                head.contains("user: octocat"),
                "{w}: base always whole: {head:?}"
            );
            let shown = facts.iter().take_while(|f| head.contains(**f)).count();
            assert!(
                facts[shown..]
                    .iter()
                    .all(|f| !head.contains(f.split(' ').next().unwrap())),
                "{w}: no half facts: {head:?}"
            );
            assert!(shown <= last, "{w}: monotone");
            last = shown;
        }
        assert!(last < facts.len(), "not everything fits at 50 columns");
        let s = render_app(&a, 120, 24);
        assert!(
            line_of(&s, "cli/cli").contains("74 open PRs"),
            "everything fits at 120"
        );
        // a base that alone is too long is ellipsized, never run into the badge
        a.header = format!(" {}", "x".repeat(80));
        let head = render_app(&a, 60, 24)
            .lines()
            .next()
            .unwrap()
            .trim_end()
            .to_string();
        assert!(
            head.contains("\u{2026}") && head.ends_with("\u{2709} 1"),
            "{head:?}"
        );
    }

    #[test]
    fn panel_titles_degrade_by_whole_segments() {
        let mut a = app(false, IconSet::Unicode);
        let i = a.panel_idx_for_test(crate::app::PK::Prs);
        a.panels[i].items = items(Kind::Pr, 12);
        for w in 50u16..=120 {
            let s = render_app(&a, w, 24);
            let t = line_of(&s, "[1]");
            let title = t.split('\u{2502}').next().unwrap_or(&t);
            assert_eq!(
                title.matches('(').count(),
                title.matches(')').count(),
                "{w}: a count was cut in half: {title:?}"
            );
            assert!(
                title.contains("12") || title.contains("Mine 12"),
                "{w}: the count survives: {title:?}"
            );
        }
        // the ladder, widest first
        let s = render_app(&a, 120, 24);
        assert!(
            line_of(&s, "[1]").contains("Mine 12 \u{b7}"),
            "every tab with its count"
        );
        let s = render_app(&a, 80, 24);
        assert!(
            line_of(&s, "[1]").contains("[1] Pull requests (12)"),
            "{}",
            line_of(&s, "[1]")
        );
        let s = render_app(&a, 50, 24);
        assert!(
            line_of(&s, "[1]").contains("[1] PRs (12)"),
            "{}",
            line_of(&s, "[1]")
        );
    }

    #[test]
    fn empty_detail_pane_describes_the_repo() {
        let mut a = app(false, IconSet::Unicode);
        assert!(
            render_app(&a, 120, 40).contains("Detail"),
            "nothing known yet"
        );
        a.meta = Some(crate::gh::RepoMeta {
            stars: 46514,
            forks: 9114,
            issues: 1038,
            branch: "trunk".into(),
            description: "GitHub's official command line tool".into(),
            license: "MIT License".into(),
            topics: vec!["cli".into(), "golang".into()],
            pushed: "2026-10-02".into(),
            ..Default::default()
        });
        let s = render_app(&a, 120, 40);
        for want in [
            "official command line tool",
            "stars           46514",
            "forks           9114",
            "open issues     1038",
            "default branch  trunk",
            "MIT License",
            "cli, golang",
            "2026-10-02",
        ] {
            assert!(s.contains(want), "missing {want:?}\n{s}");
        }
        a.panels[0].items = items(Kind::Pr, 1);
        let s = render_app(&a, 120, 40);
        assert!(
            !s.contains("MIT License") && s.contains("Overview"),
            "an item replaces it\n{s}"
        );
    }

    fn low_rate(remaining: u32, reset: u64) -> crate::rate::RateState {
        crate::rate::RateState {
            graphql: Some(crate::rate::Bucket {
                limit: 5000,
                remaining,
                reset,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn low_quota_shows_a_chip_and_a_pause_note() {
        let later = crate::rate::now() + 3600;
        for (w, h) in [(80u16, 24u16), (120, 40)] {
            let mut a = app(false, IconSet::Unicode);
            a.seed_unread(&["a/b"]);
            let plain = render_app(&a, w, h);
            assert!(!plain.contains('\u{26a1}'), "no chip with plenty of quota");
            a.rate = low_rate(412, later);
            let s = render_app(&a, w, h);
            let head = s.lines().next().unwrap().trim_end().to_string();
            assert!(
                head.contains('\u{26a1}') && head.contains("412/5000"),
                "{w}: {head:?}"
            );
            assert!(
                head.ends_with("\u{2709} 1"),
                "the badge keeps the edge next to the chip: {head:?}"
            );
            // not paused at 8%: no note; the bar is untouched
            assert!(!s.contains("paused background"));
            // paused: the note leads the bar and names when it ends
            a.rate = low_rate(300, later);
            a.paused = true;
            a.rate_clock = Some((later, "14:05".into()));
            let s = render_app(&a, w, h);
            let bar = s.lines().last().unwrap();
            assert!(
                bar.contains("paused background refresh (rate limit low, resets 14:05)"),
                "{w}: {bar:?}"
            );
            // [api] rate_header = false hides the chip but not the pause
            a.cfg.api.rate_header = false;
            let s = render_app(&a, w, h);
            assert!(!s.contains('\u{26a1}') && s.contains("paused background"));
        }
        let mut a = app(false, IconSet::Ascii);
        a.rate = low_rate(412, crate::rate::now() + 60);
        assert!(
            render_app(&a, 120, 24)
                .lines()
                .next()
                .unwrap()
                .contains("! 412/5000")
        );
    }

    #[test]
    fn counts_that_are_not_fetched_by_themselves_show_a_question_mark() {
        let a = app(false, IconSet::Unicode);
        let s = render_app(&a, 120, 40);
        let prs = line_of(&s, "[1]");
        assert!(
            prs.contains("Rev ?"),
            "search-backed tabs wait to be opened: {prs}"
        );
        assert!(
            prs.contains("All \u{2026}") && prs.contains("Mrg \u{2026}"),
            "plain lists are on their way: {prs}"
        );
        let mut off = app(false, IconSet::Unicode);
        off.cfg.api.counts = crate::config::Counts::Off;
        let s = render_app(&off, 120, 40);
        assert!(
            line_of(&s, "[5]").contains("Tags ?"),
            "{}",
            line_of(&s, "[5]")
        );
        let mut g = app(false, IconSet::Unicode);
        g.global = true;
        assert!(line_of(&render_app(&g, 120, 40), "[3]").contains("All ?"));
    }

    #[test]
    fn panels_that_have_not_been_focused_say_so_instead_of_claiming_to_be_empty() {
        let mut a = app(false, IconSet::Unicode);
        let i = a.panel_idx_for_test(crate::app::PK::Repo);
        a.panels[i].unloaded = true;
        let s = render_app(&a, 120, 40);
        assert!(
            s.contains("loads when you focus it") && !line_of(&s, "[5]").contains("Branches 0"),
            "{s}"
        );
        key(&mut a, KeyCode::Char('5'));
        assert!(!a.panels[i].unloaded, "focusing loads it");
    }

    #[test]
    fn a_failed_tab_shows_a_cross_not_zero() {
        let mut a = app(false, IconSet::Unicode);
        a.panels[0].error = Some("gh timed out after 60s".into());
        a.count_failed.insert((crate::app::PK::Prs, 2));
        for (w, h) in [(120u16, 40u16), (80, 24)] {
            let s = render_app(&a, w, h);
            let t = line_of(&s, "[1]");
            assert!(
                t.contains('\u{2717}') && !t.contains("Mine 0") && !t.contains("(0)"),
                "{w}: {t}"
            );
        }
        let t = line_of(&render_app(&a, 120, 40), "[1]");
        assert!(
            t.matches('\u{2717}').count() == 2,
            "the active tab and the failed count: {t}"
        );
        let mut ascii = app(false, IconSet::Ascii);
        ascii.panels[0].error = Some("x".into());
        assert!(line_of(&render_app(&ascii, 120, 40), "[1]").contains("Mine x"));
    }

    fn home_app() -> App {
        let mut a = App::build_start(
            Some("o/r".into()),
            true,
            Theme::new(false, IconSet::Unicode, true),
            false,
            crate::config::Config::default(),
        );
        let it = |repo: &str, n: u64, title: &str, kind: Kind| Item {
            number: n,
            title: title.into(),
            repo: repo.into(),
            state: "open".into(),
            kind,
            ..Default::default()
        };
        a.panels[0].items = vec![
            it("cli/cli", 14577, "Bump go-runewidth", Kind::Pr),
            it("acme/widgets", 7, "Fix the flaky retry", Kind::Pr),
        ];
        a.panels[0].hidden = 3;
        a.panels[1].items = vec![it("cli/cli", 99, "Mine one", Kind::Pr)];
        a.panels[2].items = vec![it("acme/widgets", 12, "Crash on start", Kind::Issue)];
        a
    }

    #[test]
    fn the_global_home_lays_out_at_both_sizes() {
        for (w, h) in [(80u16, 24u16), (120, 40)] {
            let a = home_app();
            let s = render_app(&a, w, h);
            let head = s.lines().next().unwrap();
            assert!(
                head.contains("all repos") && head.contains("scope: all"),
                "{w}x{h}: {head:?}"
            );
            for want in ["[1] ", "[2] ", "[3] ", "[4] "] {
                assert!(s.contains(want), "{w}x{h} missing {want:?}\n{s}");
            }
            for gone in ["[5] ", "Branches", "Actions", "Notifications"] {
                assert!(!s.contains(gone), "{w}x{h} has {gone:?}\n{s}");
            }
            assert!(s.contains("Review requested"), "{w}x{h}\n{s}");
            // rows carry owner/repo#N with the title; hidden repos are counted in the title
            assert!(s.contains("cli/cli#14577 Bump"), "{w}x{h}\n{s}");
            assert!(s.contains("acme/widgets#7"), "{w}x{h}\n{s}");
            assert!(
                line_of(&s, "[1]").contains("3 hidden") || w == 80,
                "{w}: {}",
                line_of(&s, "[1]")
            );
            // unopened search tabs show `?`, never a made-up number
            assert!(
                line_of(&s, "[2]").contains('?') || w == 80,
                "{}",
                line_of(&s, "[2]")
            );
        }
        // the repo home next to it is unchanged
        let s = render_app(&app(false, IconSet::Unicode), 120, 40);
        assert!(s.contains("[4] Actions") && !s.contains("Review requested"));
    }

    #[test]
    fn the_global_home_has_no_repo_facts_and_keeps_the_inbox_badge() {
        let mut a = home_app();
        a.meta = Some(crate::gh::RepoMeta {
            stars: 5,
            ..Default::default()
        });
        a.seed_unread(&["a/b", "c/d"]);
        let head = render_app(&a, 120, 40).lines().next().unwrap().to_string();
        assert!(head.trim_end().ends_with("\u{2709} 2"), "{head:?}");
        let s = render_app(&a, 120, 40);
        assert!(!head.contains('\u{2605}') || a.global, "{head:?}");
        assert!(
            s.contains("s scope") && s.contains("S open repo"),
            "the bar teaches the new keys\n{s}"
        );
    }

    #[test]
    fn the_scope_picker_renders_and_marks_the_current_scope() {
        let mut a = home_app();
        a.scope = crate::global::Scope::Favorites;
        key(&mut a, KeyCode::Char('s'));
        let s = render_app(&a, 80, 24);
        for want in ["Scope", "All repos", "Favorites (0)", "Org...", "Repo..."] {
            assert!(s.contains(want), "{want}\n{s}");
        }
        assert!(
            s.contains("\u{25cf} Favorites"),
            "current scope marked\n{s}"
        );
        key(&mut a, KeyCode::Char('j'));
        key(&mut a, KeyCode::Char('j'));
        key(&mut a, KeyCode::Enter);
        let s = render_app(&a, 80, 24);
        assert!(
            s.contains("organization") && s.contains("loading your organizations"),
            "{s}"
        );
    }

    #[test]
    fn the_repos_panel_bar_names_help_once() {
        for global in [true, false] {
            let mut a = if global {
                home_app()
            } else {
                let cfg =
                    crate::config::parse("[panels]\nshow = [\"prs\", \"repos\"]\n", "t").unwrap();
                App::build(
                    "o/r".into(),
                    Theme::new(false, IconSet::Unicode, true),
                    false,
                    cfg,
                )
            };
            key(&mut a, KeyCode::Char(if global { '4' } else { '2' }));
            let s = render_app(&a, 120, 30);
            let bar = s.lines().last().unwrap();
            assert_eq!(bar.matches("help").count(), 1, "global={global}: {bar:?}");
            assert!(
                bar.contains("Enter open repo") && bar.contains("H hide"),
                "{bar:?}"
            );
        }
    }

    #[test]
    fn a_favorites_scope_without_favorites_says_what_to_do() {
        let mut a = home_app();
        a.scope = crate::global::Scope::Favorites;
        a.panels[0].items.clear();
        let s = render_app(&a, 120, 40);
        assert!(
            s.contains("No favorites yet") && s.contains("(repo browser) to add some"),
            "wrapped, not cut off\n{s}"
        );
        a.cfg.repos.favorites = vec!["a/b".into()];
        let s = render_app(&a, 120, 40);
        assert!(s.contains("nothing here") && !s.contains("No favorites yet"));
    }

    #[test]
    fn an_unopened_search_tab_reads_the_same_at_every_width() {
        for (w, h) in [(80u16, 24u16), (120, 40)] {
            let mut a = home_app();
            a.panels[1].items.clear();
            a.panels[1].unloaded = true;
            let t = line_of(&render_app(&a, w, h), "[2]");
            assert!(t.contains('?') && !t.contains('\u{2026}'), "{w}: {t}");
        }
    }

    #[test]
    fn hostile_repo_names_and_titles_keep_every_border_in_place() {
        let mut a = home_app();
        let mut bad = Item {
            number: 1,
            repo: "ev\u{202e}il/re\u{200b}po\u{2066}".into(),
            title: "line1\r\nline2\u{00AD}\u{2028}\u{3164}\u{E0001}\u{FE00}tail".into(),
            state: "open".into(),
            kind: Kind::Pr,
            ..Default::default()
        };
        crate::gh::clean_item(&mut bad);
        a.panels[0].items = vec![bad];
        for (w, h) in [(80u16, 24u16), (120, 40)] {
            let s = render_app(&a, w, h);
            let lines: Vec<Vec<char>> = s.lines().map(|l| l.chars().collect()).collect();
            let top = lines.iter().find(|l| l.contains(&'\u{256e}')).unwrap();
            let col = top
                .iter()
                .position(|c| *c == '\u{256e}')
                .expect("the panel's top-right corner");
            let row = lines
                .iter()
                .find(|l| l.iter().collect::<String>().contains("<U+202E>"))
                .expect("the hostile row");
            assert_eq!(
                row[col], '\u{2502}',
                "{w}x{h}: the border stayed in its column\n{s}"
            );
            assert!(
                row.iter().collect::<String>().contains("line1 line2"),
                "{w}x{h}\n{s}"
            );
        }
    }
}
