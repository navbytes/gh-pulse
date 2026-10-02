use crate::act::{self, Action, Sel};
use crate::diff::{self, DiffMode};
use crate::gh::{self, Data, Item, Kind, Tab};
use crate::syn::Hl;
use crate::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

/// Panel identity. Files / Checks / Comments are derived from the selected (or drilled-into) PR.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PK {
    Status,
    Prs,
    Files,
    Checks,
    Comments,
    Issues,
    Actions,
    Branches,
    Releases,
    Notifs,
}

impl PK {
    /// Index understood by `gh::list`; None for derived panels.
    pub fn src(self) -> Option<usize> {
        Some(match self {
            PK::Status => 0,
            PK::Prs => 1,
            PK::Issues => 2,
            PK::Actions => 3,
            PK::Branches => 4,
            PK::Releases => 5,
            PK::Notifs => 6,
            _ => return None,
        })
    }

    pub fn derived(self) -> bool {
        self.src().is_none()
    }
}

/// "Drill-in" mode: the left column shows the PR's own Files / Checks / Comments.
pub struct Ctx {
    pub pr: Item,
    saved: Vec<Panel>,
    saved_focus: usize,
}

pub struct Panel {
    pub kind: PK,
    pub title: &'static str,
    pub tabs: &'static [&'static str],
    pub tab: usize,
    pub items: Vec<Item>,
    pub loading: bool,
    pub error: Option<String>,
    pub cursor: usize,
    seq: u64,
}

impl Panel {
    fn new(kind: PK, title: &'static str, tabs: &'static [&'static str]) -> Self {
        Panel {
            kind,
            title,
            tabs,
            tab: 0,
            items: vec![],
            loading: false,
            error: None,
            cursor: 0,
            seq: 0,
        }
    }
}

pub enum Load {
    Loading,
    Done(Result<Data, String>),
}

pub struct Confirm {
    pub cmd: Vec<String>,
    local_of: Option<String>,
    pub scroll: u16,
    /// Set by the renderer so scrolling can't overshoot.
    pub max_scroll: Cell<u16>,
}

impl Confirm {
    fn new(cmd: Vec<String>, local_of: Option<String>) -> Self {
        Confirm {
            cmd,
            local_of,
            scroll: 0,
            max_scroll: Cell::new(0),
        }
    }
}

pub enum Modal {
    Menu(Vec<Action>, usize),
    Input {
        title: &'static str,
        buf: String,
        required: bool,
        build: act::Build,
    },
    Confirm(Confirm),
    /// None while `gh repo list` is loading.
    Repos(Option<Vec<String>>, usize),
}

/// Screen regions recorded by the last draw, for mouse hit-testing.
#[derive(Default, Clone)]
pub struct Hit {
    pub panels: Vec<Rect>,
    pub list: Rect,
    pub list_off: usize,
    pub detail: Rect,
    pub body: Rect,
    pub tab_y: u16,
    pub tabs: Vec<(u16, u16)>,
}

pub struct Log {
    pub title: String,
    pub lines: Vec<String>,
}

enum Msg {
    // Results carry the generation they were requested in; stale ones are dropped.
    List(PK, usize, u64, Result<Vec<Item>, String>),
    Detail(String, Tab, u64, Result<Data, String>),
    Log(String, String, u64, Result<String, String>),
    Status(Result<String, String>),
    Done(Result<String, String>),
    Repos(u64, Result<Vec<String>, String>),
    Header(String),
}

pub struct App {
    pub theme: Theme,
    pub hit: RefCell<Hit>,
    pub tick: usize,
    pub hl: RefCell<Option<Hl>>,
    pub repo: String,
    pub header: String,
    pub panels: Vec<Panel>,
    pub focus: usize,
    pub filter: String,
    pub typing: bool,
    pub help: bool,
    pub help_scroll: usize,
    /// Set by the renderer: how far the help popup can scroll.
    pub help_max: Cell<usize>,
    pub global: bool,
    pub show_log: bool,
    pub modal: Option<Modal>,
    pub status: String,
    pub detail_focus: bool,
    dtab: usize,
    pub row: usize,
    pub ctx: Option<Ctx>,
    pub zoom: bool,
    /// (PR key, path) pairs the user marked viewed; in-memory only.
    pub viewed: HashSet<(String, String)>,
    pub mode: DiffMode,
    pub wrap: bool,
    /// Whether the last diff render was side-by-side (row counts depend on it).
    pub eff_split: Cell<bool>,
    /// Display row where each logical diff row starts (soft-wrap makes them differ).
    pub row_starts: RefCell<Vec<usize>>,
    // Written by the renderer, which only has &App; nav() clamps against them.
    pub scroll: Cell<usize>,
    pub view_max: Cell<usize>,
    pub view_h: Cell<usize>,
    pub log: Option<Log>,
    cache: HashMap<(String, Tab), Load>,
    /// Bumped whenever `cache` is cleared or the repo scope changes.
    dgen: u64,
    repos_seq: u64,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

fn derive_items(kind: PK, pr: &Item, d: &Data) -> Vec<Item> {
    let base = |title: String| Item {
        title,
        repo: pr.repo.clone(),
        ..Default::default()
    };
    match (kind, d) {
        (PK::Files, Data::Files(fd)) => fd
            .files
            .iter()
            .map(|f| Item {
                kind: Kind::File,
                state: f.status.to_string(),
                fm: (
                    f.adds,
                    f.dels,
                    fd.threads.get(&f.path).copied().unwrap_or(0),
                ),
                ..base(f.path.clone())
            })
            .collect(),
        (PK::Checks, Data::Checks(c)) => c
            .iter()
            .map(|c| {
                let wf = c.workflow.as_deref().filter(|w| !w.is_empty());
                let dur = gh::duration(c).unwrap_or_default();
                Item {
                    kind: Kind::Check,
                    state: c.bucket.clone(),
                    url: c.link.clone(),
                    meta: format!("{}{dur}", wf.map(|w| format!("{w} ")).unwrap_or_default()),
                    ..base(c.name.clone())
                }
            })
            .collect(),
        (PK::Comments, Data::Comments(e)) => e
            .iter()
            .map(|e| Item {
                kind: Kind::Comment,
                state: if e.resolved {
                    "resolved".into()
                } else {
                    String::new()
                },
                body: e.body.clone(),
                ..base(e.head.clone())
            })
            .collect(),
        _ => vec![],
    }
}

fn step(cur: usize, d: isize, max: usize) -> usize {
    cur.min(max).saturating_add_signed(d).min(max)
}

impl App {
    pub fn new(repo: String, theme: Theme) -> Self {
        Self::with(repo, theme, true)
    }

    /// `load: false` skips all `gh` calls (layout tests).
    pub fn with(repo: String, theme: Theme, load: bool) -> Self {
        let panel = Panel::new;
        let (tx, rx) = channel();
        let mut app = App {
            theme,
            hit: RefCell::default(),
            tick: 0,
            hl: RefCell::new(None),
            header: format!(" {repo}"),
            repo,
            panels: vec![
                panel(PK::Status, "Status", &[]),
                panel(
                    PK::Prs,
                    "Pull requests",
                    &["Mine", "Review requested", "All open", "Merged"],
                ),
                panel(PK::Files, "Files", &[]),
                panel(PK::Issues, "Issues", &["Assigned", "Mine", "All open"]),
                panel(PK::Actions, "Actions", &["Runs", "Workflows"]),
                panel(PK::Branches, "Branches", &[]),
                panel(PK::Releases, "Releases", &[]),
                panel(PK::Notifs, "Notifications", &[]),
            ],
            focus: 0,
            filter: String::new(),
            typing: false,
            help: false,
            help_scroll: 0,
            help_max: Cell::new(0),
            global: false,
            show_log: false,
            modal: None,
            status: String::new(),
            detail_focus: false,
            dtab: 0,
            row: 0,
            ctx: None,
            zoom: false,
            viewed: HashSet::new(),
            mode: DiffMode::Auto,
            wrap: true,
            eff_split: Cell::new(false),
            row_starts: RefCell::new(vec![]),
            scroll: Cell::new(0),
            view_max: Cell::new(0),
            view_h: Cell::new(10),
            log: None,
            cache: HashMap::new(),
            dgen: 0,
            repos_seq: 0,
            tx,
            rx,
        };
        if load {
            for i in 0..app.panels.len() {
                app.load_panel(i);
            }
            app.spawn_header();
        }
        app
    }

    fn spawn_header(&mut self) {
        let (tx, repo) = (self.tx.clone(), self.repo.clone());
        thread::spawn(move || {
            // The cwd's branch only describes `repo` when cwd is a clone of it.
            let local = gh::repo_here().is_ok_and(|h| h.eq_ignore_ascii_case(&repo));
            let branch = Command::new("git")
                .args(["branch", "--show-current"])
                .output();
            let branch = branch.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
            let branch = if local {
                branch.unwrap_or_default()
            } else {
                String::new()
            };
            let _ = tx.send(Msg::Header(format!(
                " {repo}  branch: {branch}  user: {}",
                gh::user()
            )));
        });
    }

    fn clear_cache(&mut self) {
        self.cache.clear();
        if let Some(h) = self.hl.get_mut() {
            h.clear();
        }
        self.dgen += 1;
    }

    fn reload_all(&mut self) {
        self.clear_cache();
        (0..self.panels.len()).for_each(|i| self.load_panel(i));
    }

    fn load_panel(&mut self, i: usize) {
        let p = &mut self.panels[i];
        let Some(src) = p.kind.src() else { return };
        p.loading = true;
        p.error = None;
        p.seq += 1;
        let (seq, kind) = (p.seq, p.kind);
        let (tx, repo, tab, global) = (self.tx.clone(), self.repo.clone(), p.tab, self.global);
        thread::spawn(move || {
            let _ = tx.send(Msg::List(kind, tab, seq, gh::list(&repo, global, src, tab)));
        });
    }

    /// The panel of this kind, in the active set or the one parked while drilled in.
    fn panel_mut(&mut self, k: PK) -> Option<&mut Panel> {
        let parked = self.ctx.as_mut().map(|c| &mut c.saved);
        self.panels
            .iter_mut()
            .chain(parked.into_iter().flatten())
            .find(|p| p.kind == k)
    }

    fn panel_idx(&self, k: PK) -> Option<usize> {
        self.panels.iter().position(|p| p.kind == k)
    }

    pub fn poll(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::List(kind, tab, seq, res) => {
                    let Some(p) = self.panel_mut(kind) else {
                        continue;
                    };
                    if p.tab != tab || p.seq != seq {
                        continue;
                    }
                    p.loading = false;
                    match res {
                        Ok(items) => {
                            p.cursor = p.cursor.min(items.len().saturating_sub(1));
                            p.items = items;
                        }
                        Err(e) => p.error = Some(e),
                    }
                }
                Msg::Detail(key, tab, g, res) => {
                    if g == self.dgen {
                        self.cache.insert((key, tab), Load::Done(res));
                    }
                }
                Msg::Log(_, key, g, _)
                    if g != self.dgen || self.selected().map(Item::key) != Some(key.clone()) => {}
                Msg::Log(title, _, _, Ok(text)) => {
                    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
                    lines.drain(..lines.len().saturating_sub(5000));
                    self.log = Some(Log { title, lines });
                    self.scroll.set(usize::MAX); // failures are at the end
                }
                Msg::Log(_, _, _, Err(e)) | Msg::Status(Err(e)) => self.status = e,
                Msg::Status(Ok(s)) => self.status = s,
                Msg::Done(res) => {
                    self.status = res.unwrap_or_else(|e| e);
                    self.reload_all();
                }
                Msg::Repos(g, _) if g != self.repos_seq => {}
                Msg::Repos(_, Ok(v)) => {
                    if let Some(Modal::Repos(slot @ None, _)) = &mut self.modal {
                        *slot = Some(v);
                    }
                }
                Msg::Repos(_, Err(e)) => (self.modal, self.status) = (None, e),
                Msg::Header(h) => self.header = h,
            }
        }
        self.sync_derived();
    }

    /// Test helper: one PR in the PR panel with its detail data already cached.
    #[cfg(test)]
    pub fn seed(&mut self, pr: Item, data: Vec<(Tab, Data)>) {
        let i = self.panel_idx(PK::Prs).unwrap();
        self.panels[i].items = vec![pr.clone()];
        for (t, d) in data {
            self.cache.insert((pr.key(), t), Load::Done(Ok(d)));
        }
        self.sync_derived();
    }

    fn fetch(&mut self, it: Item, tab: Tab) {
        if !gh::needs_fetch(it.kind, tab) {
            return;
        }
        let key = (it.key(), tab);
        if self.cache.contains_key(&key) {
            return;
        }
        self.cache.insert(key.clone(), Load::Loading);
        let (tx, repo, g) = (self.tx.clone(), self.repo.clone(), self.dgen);
        thread::spawn(move || {
            let _ = tx.send(Msg::Detail(key.0, tab, g, gh::detail(&repo, &it, tab)));
        });
    }

    /// Fetches whatever the screen needs and isn't cached yet; called when input is idle.
    pub fn ensure(&mut self) {
        let kind = self.panels[self.focus].kind;
        // The PR behind Files/Checks/Comments: only fetched while the user is working on PRs.
        if let Some(pr) = self.pr_item().filter(|p| p.kind == Kind::Pr).cloned()
            && (self.ctx.is_some() || matches!(kind, PK::Prs | PK::Files))
        {
            self.fetch(pr.clone(), Tab::Diff);
            if self.ctx.is_some() {
                self.fetch(pr.clone(), Tab::Checks);
                self.fetch(pr, Tab::Comments);
            }
        }
        if kind == PK::Checks
            && let Some(c) = self.selected_in(self.focus).cloned()
        {
            self.fetch(c, Tab::Logs);
        }
        if kind.derived() && !(kind == PK::Files && self.ctx.is_none()) {
            return;
        }
        if let Some(it) = self.selected().cloned() {
            let tab = self.cur_tab();
            self.fetch(it, tab);
        }
    }

    pub fn load(&self) -> Option<&Load> {
        self.cache.get(&(self.selected()?.key(), self.cur_tab()))
    }

    pub fn data(&self) -> Option<&Data> {
        match self.load()? {
            Load::Done(Ok(d)) => Some(d),
            _ => None,
        }
    }

    /// Cached data for any item/tab (the right pane of Checks/Comments panels).
    pub fn data_of(&self, it: &Item, tab: Tab) -> Option<&Load> {
        self.cache.get(&(it.key(), tab))
    }

    pub fn visible(&self, i: usize) -> Vec<&Item> {
        let p = &self.panels[i];
        if p.kind.derived() {
            return p.items.iter().collect();
        }
        let f = self.filter.to_lowercase();
        let hit = |it: &&Item| {
            it.title.to_lowercase().contains(&f) || it.meta.to_lowercase().contains(&f)
        };
        p.items.iter().filter(hit).collect()
    }

    pub fn selected_in(&self, i: usize) -> Option<&Item> {
        let v = self.visible(i);
        v.get(self.panels[i].cursor.min(v.len().saturating_sub(1)))
            .copied()
    }

    /// The pull request the Files/Checks/Comments panels describe.
    pub fn pr_item(&self) -> Option<&Item> {
        match &self.ctx {
            Some(c) => Some(&c.pr),
            None => self
                .panel_idx(PK::Prs)
                .and_then(|i| self.selected_in(i))
                .filter(|p| p.kind == Kind::Pr),
        }
    }

    /// Focused panel kind, when it is one of the derived ones.
    pub fn dk(&self) -> Option<PK> {
        let k = self.panels[self.focus].kind;
        k.derived().then_some(k)
    }

    /// What detail operations apply to: the PR when a derived panel has focus.
    pub fn selected(&self) -> Option<&Item> {
        if self.dk().is_some() {
            self.pr_item()
        } else {
            self.selected_in(self.focus)
        }
    }

    pub fn tabs(&self) -> &'static [Tab] {
        gh::tabs(self.selected().map_or(Kind::Status, |i| i.kind))
    }

    pub fn cur_tab(&self) -> Tab {
        match self.dk() {
            Some(PK::Files) if self.ctx.is_some() => Tab::Diff,
            Some(PK::Checks) => Tab::Checks,
            Some(PK::Comments) => Tab::Comments,
            _ => {
                let t = self.tabs();
                t[self.dtab.min(t.len() - 1)]
            }
        }
    }

    pub fn dtab(&self) -> usize {
        let cur = self.cur_tab();
        self.tabs().iter().position(|t| *t == cur).unwrap_or(0)
    }

    /// Cursor of the Files panel = the file shown in the diff.
    pub fn file(&self) -> usize {
        self.panel_idx(PK::Files)
            .map_or(0, |i| self.panels[i].cursor)
    }

    fn set_file(&mut self, v: usize) {
        if let Some(i) = self.panel_idx(PK::Files) {
            self.panels[i].cursor = v;
        }
    }

    /// Selected row of the Checks / Comments list (own panel in drill-in, `row` in the tabs).
    pub fn lrow(&self) -> usize {
        match self.dk() {
            Some(PK::Checks | PK::Comments) => self.panels[self.focus].cursor,
            _ => self.row,
        }
    }

    pub fn pr_key(&self) -> String {
        self.pr_item().map(Item::key).unwrap_or_default()
    }

    /// Rebuilds Files/Checks/Comments rows from the cache for the current PR.
    fn sync_derived(&mut self) {
        let pr = self.pr_item().cloned();
        for i in 0..self.panels.len() {
            let kind = self.panels[i].kind;
            if !kind.derived() {
                continue;
            }
            let tab = match kind {
                PK::Files => Tab::Diff,
                PK::Checks => Tab::Checks,
                _ => Tab::Comments,
            };
            let (mut items, mut loading, mut error) = (vec![], false, None);
            if let Some(pr) = &pr {
                match self.cache.get(&(pr.key(), tab)) {
                    Some(Load::Loading) => loading = true,
                    Some(Load::Done(Err(e))) => error = Some(e.clone()),
                    Some(Load::Done(Ok(d))) => items = derive_items(kind, pr, d),
                    None => {}
                }
            }
            let p = &mut self.panels[i];
            p.cursor = p.cursor.min(items.len().saturating_sub(1));
            (p.items, p.loading, p.error) = (items, loading, error);
        }
    }

    /// Selecting another PR restarts the Files panel at its first file.
    fn after_select(&mut self) {
        if self.panels[self.focus].kind == PK::Prs {
            self.set_file(0);
            self.sync_derived();
        }
    }

    fn reset_view(&mut self) {
        (self.row, self.log) = (0, None);
        self.scroll.set(0);
    }

    fn row_len(&self) -> usize {
        match self.data() {
            Some(Data::Checks(c)) => c.len(),
            Some(Data::Comments(e)) => e.len(),
            Some(Data::Files(fd)) => fd.files.get(self.file()).map_or(0, |f| {
                if self.eff_split.get() {
                    diff::split(&f.lines).len()
                } else {
                    f.lines.len()
                }
            }),
            _ => 0,
        }
    }

    /// Move whatever the active pane scrolls by: list cursor, row cursor, or plain scroll.
    fn nav(&mut self, d: isize) {
        self.nav_in(self.detail_focus, d);
    }

    fn nav_in(&mut self, detail: bool, d: isize) {
        if self.log.is_some() || (detail && !self.cursor_tab()) {
            let m = self.view_max.get();
            self.scroll.set(step(self.scroll.get(), d, m));
        } else if detail {
            self.row = step(self.row, d, self.row_len().saturating_sub(1));
        } else {
            let n = self.visible(self.focus).len().saturating_sub(1);
            let p = &mut self.panels[self.focus];
            p.cursor = step(p.cursor, d, n);
            self.after_select();
            self.reset_view();
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        if self.modal.is_some() || self.help {
            return;
        }
        let pos = Position {
            x: m.column,
            y: m.row,
        };
        let hit = self.hit.borrow().clone();
        let in_left = hit.panels.iter().any(|r| r.contains(pos));
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let d = if m.kind == MouseEventKind::ScrollUp {
                    -1
                } else {
                    1
                };
                if hit.detail.contains(pos) {
                    self.nav_in(true, d * 3);
                } else if in_left {
                    self.nav_in(false, d);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(i) = hit.panels.iter().position(|r| r.contains(pos)) {
                    let was_list = i == self.focus && !self.detail_focus;
                    if !was_list {
                        self.set_focus(i);
                    }
                    if was_list && hit.list.contains(pos) {
                        let r = hit.list_off + (pos.y - hit.list.y) as usize;
                        if r < self.visible(i).len() {
                            self.panels[i].cursor = r;
                            self.after_select();
                            self.reset_view();
                        }
                    }
                } else if hit.detail.contains(pos) {
                    self.detail_focus = self.selected().is_some();
                    if pos.y == hit.tab_y {
                        if let Some(i) = hit.tabs.iter().position(|&(a, b)| pos.x >= a && pos.x < b)
                        {
                            self.dtab = i;
                            self.reset_view();
                        }
                    } else if hit.body.contains(pos) && self.cursor_tab() {
                        let d = self.scroll.get() + (pos.y - hit.body.y) as usize;
                        let r = if self.cur_tab() == Tab::Diff {
                            // soft-wrapped rows: display row -> logical row
                            let st = self.row_starts.borrow();
                            st.partition_point(|&s| s <= d).saturating_sub(1)
                        } else {
                            d
                        };
                        self.row = r.min(self.row_len().saturating_sub(1));
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether the detail pane has a row cursor (vs plain scrolling).
    fn cursor_tab(&self) -> bool {
        matches!(self.cur_tab(), Tab::Checks | Tab::Comments | Tab::Diff)
            && !matches!(self.dk(), Some(PK::Checks | PK::Comments))
    }

    fn set_focus(&mut self, i: usize) {
        self.focus = i % self.panels.len();
        self.detail_focus = false;
        self.reset_view();
        // Focusing Files shows the diff; `[` `]` can then move on to the PR's other tabs.
        if self.panels[self.focus].kind == PK::Files
            && self.ctx.is_none()
            && let Some(d) = self.tabs().iter().position(|t| *t == Tab::Diff)
        {
            self.dtab = d;
        }
    }

    /// `[`/`]`: the detail pane's tabs (what the header shows), from either focus.
    fn tab_step(&mut self, d: isize) {
        if self.ctx.is_some() {
            let n = self.panels.len() as isize;
            self.set_focus((self.focus as isize + d).rem_euclid(n) as usize);
            return;
        }
        // Files in the normal layout still switches the PR's detail tabs; other derived panels have one view.
        if self.dk().is_some() && !(self.dk() == Some(PK::Files) && self.ctx.is_none()) {
            return;
        }
        self.dtab = (self.dtab() as isize + d).rem_euclid(self.tabs().len() as isize) as usize;
        self.reset_view();
    }

    /// `{`/`}`: the focused panel's own tabs (Mine / Review requested / ...).
    fn panel_tab_step(&mut self, d: isize) {
        let p = &mut self.panels[self.focus];
        if p.tabs.len() > 1 {
            (p.tab, p.cursor) = (
                (p.tab as isize + d).rem_euclid(p.tabs.len() as isize) as usize,
                0,
            );
            p.items.clear();
            self.reset_view();
            self.load_panel(self.focus);
        }
    }

    /// Returns true to quit.
    pub fn on_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') {
            return true;
        }
        self.status.clear();
        if let Some(m) = self.modal.take() {
            self.modal = self.modal_key(m, k, ctrl);
            return false;
        }
        if self.help {
            let m = self.help_max.get();
            match k.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.help_scroll = (self.help_scroll + 1).min(m)
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                _ => (self.help, self.help_scroll) = (false, 0),
            }
            return false;
        }
        if self.typing {
            match k.code {
                KeyCode::Enter => self.typing = false,
                KeyCode::Esc => (self.typing, self.filter) = (false, String::new()),
                KeyCode::Backspace => drop(self.filter.pop()),
                KeyCode::Char(c) => self.filter.push(c),
                _ => {}
            }
            self.panels.iter_mut().for_each(|p| p.cursor = 0);
            self.reset_view();
            return false;
        }
        if self.log.is_some()
            && matches!(
                k.code,
                KeyCode::Esc | KeyCode::Char('q' | 'h') | KeyCode::Left
            )
        {
            self.log = None;
            return false;
        }
        match k.code {
            KeyCode::Char('q') if self.log.is_none() => return true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('/') if !self.detail_focus => self.typing = true,
            KeyCode::Tab => self.set_focus(self.focus + 1),
            KeyCode::BackTab => self.set_focus(self.focus + self.panels.len() - 1),
            KeyCode::Char(c @ '1'..='8') if (c as usize - '1' as usize) < self.panels.len() => {
                self.set_focus(c as usize - '1' as usize)
            }
            KeyCode::Char('[') => self.tab_step(-1),
            KeyCode::Char(']') => self.tab_step(1),
            KeyCode::Char('{') => self.panel_tab_step(-1),
            KeyCode::Char('}') => self.panel_tab_step(1),
            KeyCode::Char('r') if ctrl => self.open_repos(),
            KeyCode::Char('r') => {
                self.clear_cache();
                self.load_panel(self.focus);
            }
            KeyCode::Char('R') => self.reload_all(),
            KeyCode::Char('x') => self.open_menu(None),
            KeyCode::Char('a') => self.open_menu(Some("Approve")),
            KeyCode::Char('C') => self.open_menu(Some("Comment on")),
            KeyCode::Char('m') => self.open_menu(Some("Merge")),
            KeyCode::Char('L') => self.show_log = !self.show_log,
            KeyCode::Char('G')
                if !self.detail_focus
                    && self.log.is_none()
                    && self.ctx.is_none()
                    && self.dk().is_none() =>
            {
                self.global = !self.global;
                self.reload_all();
            }
            // Derived panels and drill-in: G is "last row" like End; say why it isn't the global toggle.
            KeyCode::Char('G') if !self.detail_focus && self.log.is_none() => {
                self.nav(isize::MAX);
                self.status = "G jumps to the last row here; the global view toggles from the PR/Issues lists".into();
            }
            KeyCode::Home => self.nav(isize::MIN),
            KeyCode::End => self.nav(isize::MAX),
            KeyCode::Char('o') => self.open(),
            KeyCode::Char('y') => self.copy(),
            KeyCode::Char('c') => self.checkout(),
            KeyCode::Enter => self.enter(),
            KeyCode::Esc if self.zoom => self.zoom = false,
            KeyCode::Esc if self.detail_focus => self.detail_focus = false,
            KeyCode::Esc if self.ctx.is_some() => self.exit_ctx(),
            KeyCode::Esc => self.filter.clear(),
            KeyCode::Char('f') => self.zoom = !self.zoom,
            KeyCode::Char('w') if self.cur_tab() == Tab::Diff => self.wrap = !self.wrap,
            KeyCode::Char('v') if self.cur_tab() == Tab::Diff => self.toggle_viewed(),
            KeyCode::Char('l') | KeyCode::Right if self.selected().is_some() => {
                self.detail_focus = true
            }
            KeyCode::Char('h') | KeyCode::Left => self.detail_focus = false,
            KeyCode::Char('t') if self.cur_tab() == Tab::Diff => {
                (self.mode, self.row) = (self.mode.next(), 0);
                self.scroll.set(0);
            }
            KeyCode::Char('n') if self.cur_tab() == Tab::Diff => self.file_step(1),
            KeyCode::Char('p') if self.cur_tab() == Tab::Diff => self.file_step(-1),
            KeyCode::Char('j') | KeyCode::Down => self.nav(1),
            KeyCode::Char('k') | KeyCode::Up => self.nav(-1),
            KeyCode::Char('d') if ctrl => self.nav((self.view_h.get() / 2) as isize),
            KeyCode::Char('u') if ctrl => self.nav(-((self.view_h.get() / 2) as isize)),
            KeyCode::Char('g') => self.nav(isize::MIN),
            KeyCode::Char('G') => self.nav(isize::MAX),
            _ => {}
        }
        false
    }

    fn sel(&self) -> Sel {
        let mut sel = Sel::default();
        match (self.cur_tab(), self.data()) {
            (Tab::Checks, Some(Data::Checks(c))) => {
                sel.check_link = c.get(self.lrow()).map(|c| c.link.clone())
            }
            (Tab::Comments, Some(Data::Comments(e))) => {
                sel.thread = e
                    .get(self.lrow())
                    .and_then(|e| Some((e.thread.clone()?, e.resolved)));
            }
            (Tab::Diff, Some(Data::Files(fd))) => {
                if let Some(f) = fd
                    .files
                    .get(self.file().min(fd.files.len().saturating_sub(1)))
                {
                    let anchor = if self.eff_split.get() {
                        diff::split(&f.lines)
                            .get(self.row)
                            .and_then(|r| r.anchor(&f.lines))
                    } else {
                        f.lines.get(self.row).and_then(diff::DLine::anchor)
                    };
                    sel.inline = anchor.map(|(l, side)| (fd.sha.clone(), f.path.clone(), l, side));
                }
            }
            _ => {}
        }
        sel
    }

    /// `filter` narrows to matching actions and jumps straight in when only one matches.
    fn open_menu(&mut self, filter: Option<&str>) {
        let mut items = act::actions(
            self.selected(),
            self.panels[self.focus].kind.src().unwrap_or(99),
            &self.repo,
            &self.sel(),
        );
        if let Some(f) = filter {
            items.retain(|a| a.label.starts_with(f));
        }
        match items.len() {
            0 => self.status = "no such action here".into(),
            1 if filter.is_some() => self.modal = Self::pick(items.remove(0)),
            _ => self.modal = Some(Modal::Menu(items, 0)),
        }
    }

    fn pick(a: Action) -> Option<Modal> {
        Some(match a.prompt {
            Some((title, required)) => Modal::Input {
                title,
                buf: String::new(),
                required,
                build: a.build,
            },
            None => Modal::Confirm(Confirm::new((a.build)(""), a.local_of)),
        })
    }

    fn open_repos(&mut self) {
        self.modal = Some(Modal::Repos(None, 0));
        self.repos_seq += 1;
        let (tx, g) = (self.tx.clone(), self.repos_seq);
        thread::spawn(move || {
            let r = gh::gh([
                "repo",
                "list",
                "--limit=50",
                "--json",
                "nameWithOwner",
                "-q",
                ".[].nameWithOwner",
            ]);
            let _ = tx.send(Msg::Repos(
                g,
                r.map(|s| s.lines().map(str::to_string).collect()),
            ));
        });
    }

    /// Returns the modal to keep showing, if any.
    fn modal_key(&mut self, m: Modal, k: KeyEvent, ctrl: bool) -> Option<Modal> {
        let down = matches!(k.code, KeyCode::Char('j') | KeyCode::Down);
        let up = matches!(k.code, KeyCode::Char('k') | KeyCode::Up);
        let mv = |i: usize, n: usize| {
            if down {
                (i + 1).min(n.saturating_sub(1))
            } else if up {
                i.saturating_sub(1)
            } else {
                i
            }
        };
        match m {
            Modal::Menu(mut items, i) => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => None,
                KeyCode::Enter => Self::pick(items.swap_remove(i)),
                _ => {
                    let i = mv(i, items.len());
                    Some(Modal::Menu(items, i))
                }
            },
            Modal::Input {
                title,
                mut buf,
                required,
                build,
            } => match k.code {
                KeyCode::Esc => None,
                KeyCode::Char('s') if ctrl => {
                    if required && buf.trim().is_empty() {
                        self.status = "input is required".into();
                        Some(Modal::Input {
                            title,
                            buf,
                            required,
                            build,
                        })
                    } else {
                        Some(Modal::Confirm(Confirm::new(build(&buf), None)))
                    }
                }
                code => {
                    match code {
                        KeyCode::Enter => buf.push('\n'),
                        KeyCode::Backspace => drop(buf.pop()),
                        KeyCode::Char(c) if !ctrl => buf.push(c),
                        _ => {}
                    }
                    Some(Modal::Input {
                        title,
                        buf,
                        required,
                        build,
                    })
                }
            },
            // Only `y` runs it: Enter would also fire from a double-tapped menu Enter.
            Modal::Confirm(mut c) => match k.code {
                KeyCode::Char('y') => {
                    self.status = format!("running: {}", gh::shell(&c.cmd));
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        let r = match &c.local_of {
                            Some(repo) => gh::require_clone(repo).and_then(|()| gh::run(&c.cmd)),
                            None => gh::run(&c.cmd),
                        };
                        let _ = tx.send(Msg::Done(r));
                    });
                    None
                }
                KeyCode::Char('n') | KeyCode::Esc => None,
                _ => {
                    c.scroll = (if down {
                        c.scroll + 1
                    } else if up {
                        c.scroll.saturating_sub(1)
                    } else {
                        c.scroll
                    })
                    .min(c.max_scroll.get());
                    Some(Modal::Confirm(c))
                }
            },
            Modal::Repos(list, i) => match (k.code, &list) {
                (KeyCode::Esc | KeyCode::Char('q'), _) => None,
                (KeyCode::Enter, Some(l)) if !l.is_empty() => {
                    self.repo = l[i].clone();
                    (self.global, self.header) = (false, format!(" {}", self.repo));
                    self.panels.iter_mut().for_each(|p| p.cursor = 0);
                    self.reset_view();
                    self.reload_all();
                    self.spawn_header();
                    None
                }
                _ => {
                    let i = mv(i, list.as_ref().map_or(0, Vec::len));
                    Some(Modal::Repos(list, i))
                }
            },
        }
    }

    fn file_step(&mut self, d: isize) {
        let Some(Data::Files(fd)) = self.data() else {
            return;
        };
        let n = fd.files.len().saturating_sub(1);
        self.set_file(step(self.file(), d, n));
        self.row = 0;
        self.scroll.set(0);
    }

    fn toggle_viewed(&mut self) {
        let k = match (self.data(), self.pr_item()) {
            (Some(Data::Files(fd)), Some(pr)) => fd
                .files
                .get(self.file())
                .map(|f| (pr.key(), f.path.clone())),
            _ => None,
        };
        if let Some(k) = k
            && !self.viewed.remove(&k)
        {
            self.viewed.insert(k);
        }
    }

    /// Drill into the selected PR: the left column becomes its Files / Checks / Comments.
    fn enter_ctx(&mut self) {
        let Some(pr) = self.pr_item().cloned() else {
            return;
        };
        let ctx_panels = vec![
            Panel::new(PK::Files, "Files", &[]),
            Panel::new(PK::Checks, "Checks", &[]),
            Panel::new(PK::Comments, "Comments", &[]),
        ];
        let saved = std::mem::replace(&mut self.panels, ctx_panels);
        self.ctx = Some(Ctx {
            pr,
            saved,
            saved_focus: self.focus,
        });
        (self.focus, self.detail_focus) = (0, false);
        self.filter.clear();
        self.reset_view();
        self.sync_derived();
    }

    /// Back to the normal panels with their cursors untouched.
    fn exit_ctx(&mut self) {
        if let Some(c) = self.ctx.take() {
            self.panels = c.saved;
            (self.focus, self.detail_focus) = (c.saved_focus, false);
            self.reset_view();
        }
    }

    fn enter(&mut self) {
        if !self.detail_focus {
            if self.ctx.is_none() && self.panels[self.focus].kind == PK::Prs {
                self.enter_ctx();
            } else {
                self.detail_focus = self.selected().is_some();
            }
            return;
        }
        if self.dk().is_some() {
            return;
        }
        let (Some(it), Some(Data::Checks(c))) = (self.selected(), self.data()) else {
            return;
        };
        let Some(ch) = c.get(self.row) else { return };
        let (repo, link, title, key) =
            (it.repo.clone(), ch.link.clone(), ch.name.clone(), it.key());
        self.status = "loading log...".into();
        let (tx, g) = (self.tx.clone(), self.dgen);
        thread::spawn(move || {
            let _ = tx.send(Msg::Log(title, key, g, gh::failed_log(&repo, &link)));
        });
    }

    fn bg(&mut self, f: impl FnOnce() -> Result<String, String> + Send + 'static) {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Msg::Status(f()));
        });
    }

    fn open(&mut self) {
        let Some(url) = self.selected().map(|i| i.url.clone()) else {
            return;
        };
        let cmd = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        self.bg(move || {
            let s = Command::new(cmd)
                .arg(&url)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            match s {
                Ok(s) if s.success() => Ok(String::new()),
                _ => Err(format!("could not open {url}")),
            }
        });
    }

    fn copy(&mut self) {
        let Some(url) = self.selected().map(|i| i.url.clone()) else {
            return;
        };
        self.bg(move || {
            for c in [
                &["pbcopy"][..],
                &["wl-copy"],
                &["xclip", "-selection", "clipboard"],
            ] {
                let child = Command::new(c[0])
                    .args(&c[1..])
                    .stdin(Stdio::piped())
                    .spawn();
                if let Ok(mut ch) = child {
                    let _ = ch.stdin.take().map(|mut s| s.write_all(url.as_bytes()));
                    if ch.wait().is_ok_and(|s| s.success()) {
                        return Ok(format!("copied {url}"));
                    }
                }
            }
            Err("no clipboard tool (pbcopy/wl-copy/xclip)".into())
        });
    }

    fn checkout(&mut self) {
        match self.selected() {
            Some(it) if it.kind == Kind::Pr => {
                let (repo, n) = (it.repo.clone(), it.number);
                self.bg(move || gh::checkout(&repo, n));
            }
            _ => self.status = "checkout works on pull requests".into(),
        }
    }
}
