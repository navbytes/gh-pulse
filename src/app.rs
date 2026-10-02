use crate::act::{self, Action, Sel};
use crate::browse::{self, Browser, Out, RepoRow};
use crate::config::{self, Act, Config, Keymap};
use crate::diff::{self, DiffMode};
use crate::gh::{self, Data, FilesData, Item, Kind, Tab};
use crate::state::Viewed;
use crate::syn::Hl;
use crate::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

/// Panel identity. Files / Checks / Comments are derived from the selected (or drilled-into) PR.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PK {
    Status,
    Prs,
    Files,
    Commits,
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

// A cache entry is built once per fetch; boxing the comment data would only add indirection.
#[allow(clippy::large_enum_variant)]
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

/// One page of the repo browser's background load.
struct RepoMsg {
    viewer: String,
    rows: Vec<RepoRow>,
    last: bool,
    truncated: bool,
}

enum Msg {
    // Results carry the generation they were requested in; stale ones are dropped.
    List(PK, usize, u64, Result<Vec<Item>, String>),
    Detail(String, Tab, u64, Result<Data, String>),
    /// A later page of comments for the item with this key.
    More(String, u64, gh::More),
    Log(String, String, u64, Result<String, String>),
    Status(Result<String, String>),
    Done(Result<String, String>),
    Repos(u64, Result<RepoMsg, String>),
    Header(String),
}

pub struct App {
    pub cfg: Config,
    cfg_path: Option<PathBuf>,
    pub keys: Keymap,
    /// Full-screen repo browser while open.
    pub browser: Option<Browser>,
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
    /// File shown from the selected commit (Commits panel).
    pub cfile: usize,
    /// When the last comments page was requested (pages are at least PAGE_GAP apart).
    last_more: Option<std::time::Instant>,
    /// Comments the user expanded (long ones, details blocks) / whose reaction names are shown, per item and row.
    pub expanded: HashSet<(String, usize)>,
    pub react_open: HashSet<(String, usize)>,
    /// Files marked viewed, persisted per repo + PR + head sha (see state.rs).
    pub viewed: Viewed,
    pub mode: DiffMode,
    pub wrap: bool,
    /// Whether the last diff render was side-by-side (row counts depend on it).
    pub eff_split: Cell<bool>,
    /// Display row where each logical diff row starts (soft-wrap makes them differ).
    pub row_starts: RefCell<Vec<usize>>,
    // Written by the renderer, which only has &App; nav() clamps against them.
    pub scroll: Cell<usize>,
    pub view_max: Cell<usize>,
    pub view_len: Cell<usize>,
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
        (PK::Commits, Data::Commits(c)) => c
            .iter()
            .map(|c| Item {
                kind: Kind::Commit,
                state: c.sha.chars().take(7).collect(),
                meta: c.sha.clone(),
                url: format!("https://github.com/{}/commit/{}", pr.repo, c.sha),
                body: c.when.clone(),
                author: gh::Author {
                    login: c.author.clone(),
                },
                fm: (c.adds, c.dels, 0),
                ..base(c.subject.clone())
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
        (PK::Comments, Data::Comments(cd)) => {
            let mut v: Vec<Item> = cd
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
                .collect();
            // a last, non-comment row says that more are coming (or why they didn't)
            if cd.loading || cd.error.is_some() || cd.paused() {
                let n = cd.remaining();
                let wait = cd.wait_secs();
                let (title, body) = match (&cd.error, cd.loading) {
                    (Some(_), _) if wait > 0 => (
                        format!("\u{2026} {n} more (rate limited)"),
                        format!("GitHub rate limit \u{2014} retry in {wait}s (press m)."),
                    ),
                    (Some(e), _) => (
                        format!("\u{2026} {n} more not loaded"),
                        format!("{n} more comments were not loaded: {e}\nPress m to retry."),
                    ),
                    (None, true) => (
                        format!("\u{2026} {n} more (loading)"),
                        format!("Loading more comments... ({}/{})", cd.loaded, cd.total),
                    ),
                    (None, false) => (
                        format!("\u{2026} {n} more (m to load)"),
                        "More comments are not loaded yet: scroll down or press m.".to_string(),
                    ),
                };
                v.push(Item {
                    kind: Kind::Comment,
                    state: "more".into(),
                    body,
                    ..base(title)
                });
            }
            v
        }
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

    /// The real entry point: config and keymap come from the user's config file.
    pub fn from_config(repo: String, theme: Theme, cfg: Config, keys: Keymap) -> Self {
        let mut app = Self::new(repo, theme);
        (app.cfg, app.keys, app.cfg_path) = (cfg, keys, config::path());
        let (viewed, warn) = Viewed::load(crate::state::path());
        app.viewed = viewed;
        app.status = warn.unwrap_or_default();
        app
    }

    /// `load: false` skips all `gh` calls (layout tests).
    pub fn with(repo: String, theme: Theme, load: bool) -> Self {
        let panel = Panel::new;
        let (tx, rx) = channel();
        let mut app = App {
            cfg: Config::default(),
            cfg_path: None,
            keys: Keymap::build(&Default::default()).expect("default keys are valid"),
            browser: None,
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
            cfile: 0,
            last_more: None,
            expanded: HashSet::new(),
            react_open: HashSet::new(),
            viewed: Viewed::default(),
            mode: DiffMode::Auto,
            wrap: true,
            eff_split: Cell::new(false),
            row_starts: RefCell::new(vec![]),
            scroll: Cell::new(0),
            view_max: Cell::new(0),
            view_len: Cell::new(0),
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

    #[cfg(test)]
    pub fn panel_idx_for_test(&self, k: PK) -> usize {
        self.panel_idx(k).unwrap()
    }

    fn panel_idx(&self, k: PK) -> Option<usize> {
        self.panels.iter().position(|p| p.kind == k)
    }

    pub fn poll(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::List(kind, tab, seq, res) => {
                    let (global, hidden) = (self.global, self.cfg.repos.clone());
                    let Some(p) = self.panel_mut(kind) else {
                        continue;
                    };
                    if p.tab != tab || p.seq != seq {
                        continue;
                    }
                    p.loading = false;
                    match res {
                        Ok(mut items) => {
                            // hidden repos disappear from the cross-repo (global) results
                            let cross_repo = (global && matches!(p.kind, PK::Prs | PK::Issues))
                                || p.kind == PK::Notifs;
                            if cross_repo {
                                items.retain(|i| !hidden.is_hidden(&i.repo));
                            }
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
                Msg::More(key, g, m) => {
                    if g == self.dgen
                        && let Some(Load::Done(Ok(Data::Comments(cd)))) =
                            self.cache.get_mut(&(key, Tab::Comments))
                    {
                        cd.apply(m);
                    }
                }
                Msg::Log(_, key, g, _)
                    if g != self.dgen || self.selected().map(Item::key) != Some(key.clone()) => {}
                Msg::Log(title, _, _, Ok(text)) => {
                    let mut lines: Vec<String> = text
                        .lines()
                        .map(|l| crate::sanitize::clean(l).into_owned())
                        .collect();
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
                Msg::Repos(_, Ok(m)) => {
                    if let Some(b) = &mut self.browser {
                        if b.viewer.is_empty() {
                            b.viewer = m.viewer;
                        }
                        b.rows.extend(m.rows);
                        (b.loading, b.truncated) = (!m.last, m.truncated);
                    }
                }
                Msg::Repos(_, Err(e)) => {
                    if let Some(b) = &mut self.browser {
                        (b.loading, b.error) = (false, Some(e));
                    }
                }
                Msg::Header(h) => self.header = crate::sanitize::clean(&h).into_owned(),
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

    /// Test helper: cache the same commit diff (parsed from `diff`) for every commit row.
    #[cfg(test)]
    pub fn seed_commit_diffs(&mut self, rows: &[crate::gh::CommitRow], diff: &str) {
        let pr = self.pr_item().cloned().unwrap();
        for it in derive_items(PK::Commits, &pr, &Data::Commits(rows.to_vec())) {
            let fd = FilesData {
                sha: it.meta.clone(),
                files: diff::parse(diff),
                threads: Default::default(),
            };
            self.cache
                .insert((it.key(), Tab::Diff), Load::Done(Ok(Data::Files(fd))));
        }
    }

    fn comments_data(&self) -> Option<&gh::CommentsData> {
        let it = self.selected()?;
        match self.cache.get(&(it.key(), Tab::Comments))? {
            Load::Done(Ok(Data::Comments(cd))) => Some(cd),
            _ => None,
        }
    }

    /// Looking at comments whose later pages are still to be fetched.
    fn comments_pending(&self) -> bool {
        self.cur_tab() == Tab::Comments
            && self.comments_data().is_some_and(|cd| !cd.pend.is_empty())
    }

    /// The viewport is within 20 rows of the last loaded comment.
    fn near_comments_end(&self) -> bool {
        match self.dk() {
            Some(PK::Comments) => {
                self.panels[self.focus].cursor + 20 >= self.panels[self.focus].items.len()
            }
            _ => self.scroll.get() + self.view_h.get() + 20 >= self.view_len.get(),
        }
    }

    /// Fetch one more page of comments, never faster than PAGE_GAP and never during a rate-limit wait.
    /// `manual` (key `m`) also retries after errors and explains refusals.
    fn load_more(&mut self, manual: bool) {
        if self.last_more.is_some_and(|t| t.elapsed() < gh::PAGE_GAP) {
            return;
        }
        let Some(it) = self.selected().cloned() else {
            return;
        };
        let key = (it.key(), Tab::Comments);
        let Some(Load::Done(Ok(Data::Comments(cd)))) = self.cache.get_mut(&key) else {
            return;
        };
        if cd.loading || cd.pend.is_empty() || (!manual && cd.error.is_some()) {
            return;
        }
        if cd.wait_secs() > 0 {
            if manual {
                self.status = format!("GitHub rate limit \u{2014} retry in {}s", cd.wait_secs());
            }
            return;
        }
        (cd.loading, cd.error, cd.blocked_until) = (true, None, None);
        let pend = cd.pend.clone();
        self.last_more = Some(std::time::Instant::now());
        let (tx, g) = (self.tx.clone(), self.dgen);
        let repo = if it.repo.is_empty() {
            self.repo.clone()
        } else {
            it.repo.clone()
        };
        thread::spawn(move || {
            let pr = it.kind == Kind::Pr;
            gh::more_pages(&repo, &it.number.to_string(), pr, pend, 1, &mut |m| {
                tx.send(Msg::More(key.0.clone(), g, m)).is_ok()
            });
        });
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
            let res = gh::detail(&repo, &it, tab);
            // Comments arrive in pages: show the first at once, keep paging in this thread.
            let more = match &res {
                Ok(Data::Comments(cd)) if !cd.pend.is_empty() => Some(cd.pend.clone()),
                _ => None,
            };
            let _ = tx.send(Msg::Detail(key.0.clone(), tab, g, res));
            if let Some(pend) = more {
                let repo = if it.repo.is_empty() {
                    repo
                } else {
                    it.repo.clone()
                };
                let pr = it.kind == Kind::Pr;
                gh::more_pages(
                    &repo,
                    &it.number.to_string(),
                    pr,
                    pend,
                    gh::AUTO_PAGES,
                    &mut |m| tx.send(Msg::More(key.0.clone(), g, m)).is_ok(),
                );
            }
        });
    }

    /// Fetches whatever the screen needs and isn't cached yet; called when input is idle.
    pub fn ensure(&mut self) {
        // scrolling toward the end of loaded comments pulls the next page, one at a time
        if self.comments_data().is_some_and(gh::CommentsData::paused) && self.near_comments_end() {
            self.load_more(false);
        }
        let kind = self.panels[self.focus].kind;
        // The PR behind Files/Checks/Comments: only fetched while the user is working on PRs.
        if let Some(pr) = self.pr_item().filter(|p| p.kind == Kind::Pr).cloned()
            && (self.ctx.is_some() || matches!(kind, PK::Prs | PK::Files))
        {
            self.fetch(pr.clone(), Tab::Diff);
            if self.ctx.is_some() {
                self.fetch(pr.clone(), Tab::Commits);
                self.fetch(pr.clone(), Tab::Checks);
                self.fetch(pr, Tab::Comments);
            }
        }
        if kind == PK::Checks
            && let Some(c) = self.selected_in(self.focus).cloned()
        {
            self.fetch(c, Tab::Logs);
        }
        if kind == PK::Commits
            && let Some(c) = self.selected_in(self.focus).cloned()
        {
            self.fetch(c, Tab::Diff);
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
        let num = f.trim_start_matches('#');
        let hit = |it: &&Item| {
            it.title.to_lowercase().contains(&f)
                || it.meta.to_lowercase().contains(&f)
                // "123" or "#123" finds a PR/issue by number
                || (it.number > 0 && !num.is_empty() && it.number.to_string().contains(num))
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
            Some(PK::Commits) => Tab::Commits,
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
                PK::Commits => Tab::Commits,
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
        match self.panels[self.focus].kind {
            PK::Prs => {
                self.set_file(0);
                self.sync_derived();
            }
            PK::Commits => self.cfile = 0, // each commit's diff starts at its first file
            _ => {}
        }
    }

    fn reset_view(&mut self) {
        (self.row, self.log) = (0, None);
        self.scroll.set(0);
    }

    /// The diff being shown and which of its files: the selected PR's (Files panel / Diff tab) or,
    /// with the Commits panel focused, the selected commit's.
    pub fn diff_data(&self) -> Option<(&FilesData, usize)> {
        if self.dk() == Some(PK::Commits) {
            let c = self.selected_in(self.focus)?;
            return match self.cache.get(&(c.key(), Tab::Diff))? {
                Load::Done(Ok(Data::Files(fd))) => Some((fd, self.cfile)),
                _ => None,
            };
        }
        match self.data()? {
            Data::Files(fd) => Some((fd, self.file())),
            _ => None,
        }
    }

    pub fn diff_active(&self) -> bool {
        self.cur_tab() == Tab::Diff || self.dk() == Some(PK::Commits)
    }

    fn row_len(&self) -> usize {
        match self.data() {
            Some(Data::Checks(c)) => c.len(),
            Some(Data::Comments(e)) => e.len(),
            _ => self.diff_data().map_or(0, |(fd, fi)| {
                fd.files.get(fi).map_or(0, |f| {
                    if self.eff_split.get() {
                        diff::split(&f.lines).len()
                    } else {
                        f.lines.len()
                    }
                })
            }),
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
                        let r = if self.diff_active() {
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

    /// Detail focus on the Comments tab with its row cursor (not the single-thread pane).
    fn comment_rows(&self) -> bool {
        self.detail_focus && self.cur_tab() == Tab::Comments && self.cursor_tab()
    }

    /// Whether the detail pane has a row cursor (vs plain scrolling).
    fn cursor_tab(&self) -> bool {
        self.dk() == Some(PK::Commits)
            || (matches!(self.cur_tab(), Tab::Checks | Tab::Comments | Tab::Diff)
                && !matches!(self.dk(), Some(PK::Checks | PK::Comments)))
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
        if self.browser.is_some() {
            self.browser_key(k);
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
        if let Some(a) = self.keys.get(&k) {
            return self.run_act(a, k);
        }
        match k.code {
            KeyCode::Tab => self.set_focus(self.focus + 1),
            KeyCode::BackTab => self.set_focus(self.focus + self.panels.len() - 1),
            KeyCode::Char(c @ '1'..='8') if (c as usize - '1' as usize) < self.panels.len() => {
                self.set_focus(c as usize - '1' as usize)
            }
            KeyCode::Char('[') => self.tab_step(-1),
            KeyCode::Char(']') => self.tab_step(1),
            KeyCode::Char('{') => self.panel_tab_step(-1),
            KeyCode::Char('}') => self.panel_tab_step(1),
            KeyCode::Home => self.nav(isize::MIN),
            KeyCode::End => self.nav(isize::MAX),
            KeyCode::Enter => self.enter(),
            KeyCode::Esc if self.zoom => self.zoom = false,
            KeyCode::Esc if self.detail_focus => self.detail_focus = false,
            KeyCode::Esc if self.ctx.is_some() => self.exit_ctx(),
            KeyCode::Esc => self.filter.clear(),
            KeyCode::Char('w') if self.diff_active() => self.wrap = !self.wrap,
            KeyCode::Char('v') if self.cur_tab() == Tab::Diff => self.toggle_viewed(),
            KeyCode::Char('l') | KeyCode::Right if self.selected().is_some() => {
                self.detail_focus = true
            }
            KeyCode::Char('h') | KeyCode::Left => self.detail_focus = false,
            KeyCode::Char('t') if self.diff_active() => {
                (self.mode, self.row) = (self.mode.next(), 0);
                self.scroll.set(0);
            }
            KeyCode::Char('n') if self.diff_active() => self.file_step(1),
            KeyCode::Char('p') if self.diff_active() => self.file_step(-1),
            KeyCode::Char('j') | KeyCode::Down => self.nav(1),
            KeyCode::Char('k') | KeyCode::Up => self.nav(-1),
            KeyCode::Char('d') | KeyCode::Char('u') if ctrl && self.comment_rows() => {
                self.page_comments(if k.code == KeyCode::Char('d') { 1 } else { -1 })
            }
            KeyCode::Char('e') if self.cur_tab() == Tab::Comments => self.toggle_card(true),
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
                    sel.inline =
                        anchor.map(|(l, side)| (fd.sha.clone(), f.raw_path.clone(), l, side));
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

    fn run_act(&mut self, a: Act, k: KeyEvent) -> bool {
        match a {
            Act::Quit => return true,
            Act::Help => self.help = true,
            Act::Filter if !self.detail_focus => self.typing = true,
            Act::Filter => {}
            Act::Refresh => {
                self.clear_cache();
                self.load_panel(self.focus);
            }
            Act::RefreshAll => self.reload_all(),
            Act::Actions => self.open_menu(None),
            Act::Approve => self.open_menu(Some("Approve")),
            Act::Comment => self.open_menu(Some("Comment on")),
            // on a Comments tab with unloaded pages, `m` loads the next page instead
            Act::Merge if self.comments_pending() => self.load_more(true),
            Act::Merge => self.open_menu(Some("Merge")),
            Act::CommandLog => self.show_log = !self.show_log,
            Act::Open => self.open(),
            Act::CopyUrl => self.copy(),
            Act::Checkout => self.checkout(),
            Act::Zoom => self.zoom = !self.zoom,
            Act::Browser => self.open_browser(),
            Act::Global => {
                if !self.detail_focus
                    && self.log.is_none()
                    && self.ctx.is_none()
                    && self.dk().is_none()
                {
                    self.global = !self.global;
                    self.reload_all();
                } else if k.code == KeyCode::Char('G') {
                    // Derived panels and drill-in: G is "last row" like End; say why it isn't the toggle.
                    self.nav(isize::MAX);
                    if !self.detail_focus && self.log.is_none() {
                        self.status = "G jumps to the last row here; the global view toggles from the PR/Issues lists".into();
                    }
                } else {
                    self.status = "the global view toggles from the PR/Issues lists".into();
                }
            }
        }
        false
    }

    /// Open the full-screen repo browser and page through every repo the user can access.
    fn open_browser(&mut self) {
        self.repos_seq += 1;
        let (tx, seq) = (self.tx.clone(), self.repos_seq);
        self.browser = Some(Browser::new());
        thread::spawn(move || {
            let (mut after, mut total) = (None::<String>, 0usize);
            loop {
                match browse::fetch_page(after.as_deref()) {
                    Err(e) => {
                        let _ = tx.send(Msg::Repos(seq, Err(e)));
                        return;
                    }
                    Ok(p) => {
                        total += p.rows.len();
                        let capped = p.next.is_some() && total >= browse::MAX_REPOS;
                        let last = p.next.is_none() || capped;
                        after = p.next;
                        let m = RepoMsg {
                            viewer: p.viewer,
                            rows: p.rows,
                            last,
                            truncated: capped,
                        };
                        if tx.send(Msg::Repos(seq, Ok(m))).is_err() || last {
                            return;
                        }
                    }
                }
            }
        });
    }

    fn browser_key(&mut self, k: KeyEvent) {
        // Typing "B" into the search box must not close it; Ctrl-r still does.
        let typing = self.browser.as_ref().is_some_and(|b| b.typing);
        if self.keys.get(&k) == Some(Act::Browser)
            && (!typing || k.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.browser = None;
            return;
        }
        let Some(b) = self.browser.as_mut() else {
            return;
        };
        match b.key(k, &self.cfg.repos) {
            Out::None => {}
            Out::Close => self.browser = None,
            Out::Reload => self.open_browser(),
            Out::Switch(r) => self.switch_repo(r),
            Out::Fav(r) => {
                let on = self.cfg.repos.toggle_fav(&r);
                self.status = format!(
                    "{} {r} {} favorites",
                    if on { "added" } else { "removed" },
                    if on { "to" } else { "from" }
                );
                if !self.save_cfg() {
                    self.cfg.repos.toggle_fav(&r); // refused: keep memory and file in agreement
                }
            }
            Out::Hide(r) => {
                let on = self.cfg.repos.toggle_hidden(&r);
                self.status = format!("{} {r}", if on { "hidden:" } else { "unhidden:" });
                if !self.save_cfg() {
                    self.cfg.repos.toggle_hidden(&r);
                }
            }
        }
    }

    /// Same as starting with `-R repo`.
    fn switch_repo(&mut self, repo: String) {
        self.browser = None;
        self.exit_ctx();
        self.repo = repo;
        (self.global, self.header) = (false, format!(" {}", self.repo));
        self.panels.iter_mut().for_each(|p| p.cursor = 0);
        self.reset_view();
        self.reload_all();
        self.spawn_header();
    }

    /// False when the save was refused or failed; the status line then says why.
    fn save_cfg(&mut self) -> bool {
        let Some(path) = &self.cfg_path else {
            return true;
        };
        match config::save_to(path, &self.cfg) {
            Ok(()) => true,
            Err(e) => {
                self.status = e;
                false
            }
        }
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
        }
    }

    fn file_step(&mut self, d: isize) {
        let Some((fd, fi)) = self.diff_data() else {
            return;
        };
        let to = step(fi, d, fd.files.len().saturating_sub(1));
        if self.dk() == Some(PK::Commits) {
            self.cfile = to;
        } else {
            self.set_file(to);
        }
        self.row = 0;
        self.scroll.set(0);
    }

    /// Head sha of the PR whose Files are loaded (the viewed marks are tied to it).
    fn head_sha(&self) -> Option<(&Item, &str)> {
        let pr = self.pr_item()?;
        match self.cache.get(&(pr.key(), Tab::Diff))? {
            Load::Done(Ok(Data::Files(fd))) => Some((pr, fd.sha.as_str())),
            _ => None,
        }
    }

    pub fn is_viewed(&self, path: &str) -> bool {
        self.head_sha().is_some_and(|(pr, sha)| {
            self.viewed
                .is_viewed(&Viewed::key(&pr.repo, pr.number), sha, path)
        })
    }

    fn toggle_viewed(&mut self) {
        let target = match (self.head_sha(), self.data()) {
            (Some((pr, sha)), Some(Data::Files(fd))) => fd.files.get(self.file()).map(|f| {
                (
                    Viewed::key(&pr.repo, pr.number),
                    sha.to_string(),
                    f.path.clone(),
                )
            }),
            _ => None,
        };
        if let Some((key, sha, path)) = target
            && let Err(e) = self.viewed.toggle(&key, &sha, &path)
        {
            self.status = e;
        }
    }

    /// Drill into the selected PR: the left column becomes its Files / Checks / Comments.
    fn enter_ctx(&mut self) {
        let Some(pr) = self.pr_item().cloned() else {
            return;
        };
        let ctx_panels = vec![
            Panel::new(PK::Files, "Files", &[]),
            Panel::new(PK::Commits, "Commits", &[]),
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

    fn toggle_card(&mut self, reactions: bool) {
        let key = (
            self.selected().map(Item::key).unwrap_or_default(),
            self.lrow(),
        );
        let set = if reactions {
            &mut self.react_open
        } else {
            &mut self.expanded
        };
        if !set.remove(&key) {
            set.insert(key);
        }
    }

    /// Half a page in the Comments tab, moving by whole comments.
    fn page_comments(&mut self, dir: isize) {
        let st = self.row_starts.borrow().clone();
        if st.is_empty() {
            return;
        }
        let half = (self.view_h.get() / 2).max(1);
        let row = self.row.min(st.len() - 1);
        let cur = st[row];
        // A comment taller than the pane is read in place first: scroll within it, then move on.
        let end = st.get(row + 1).map_or(self.view_len.get(), |n| n - 1); // exclusive, minus the gap line
        let (top, h) = (self.scroll.get(), self.view_h.get());
        if end > cur + h {
            if dir > 0 && top + h < end {
                self.scroll.set((top + half).min(end - h));
                return;
            }
            if dir < 0 && top > cur {
                self.scroll.set(top.saturating_sub(half).max(cur));
                return;
            }
        }
        let target = if dir > 0 {
            cur + half
        } else {
            cur.saturating_sub(half)
        };
        let r = st.partition_point(|&s| s <= target).saturating_sub(1);
        let r = if dir > 0 {
            r.max(self.row + 1)
        } else {
            r.min(self.row.saturating_sub(1))
        };
        self.row = r.min(st.len() - 1);
    }

    fn enter(&mut self) {
        // Comments: Enter expands the selected comment (also straight from the drill-in list)
        if self.dk() == Some(PK::Comments) || (self.detail_focus && self.cur_tab() == Tab::Comments)
        {
            self.toggle_card(false);
            return;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::IconSet;

    #[test]
    fn refused_save_leaves_favorites_and_hidden_unchanged() {
        let dir = std::env::temp_dir().join(format!("gh-pulse-app-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "repos = [broken\n").unwrap();
        let mut a = App::with(
            "o/r".into(),
            Theme::new(false, IconSet::Unicode, true),
            false,
        );
        a.cfg_path = Some(path.clone());
        let mut b = Browser::new();
        b.rows = vec![RepoRow {
            name: "o/a".into(),
            owner: "o".into(),
            ..Default::default()
        }];
        (b.typing, b.viewer) = (false, "o".into());
        a.browser = Some(b);
        let press = |a: &mut App, c| a.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        press(&mut a, 'f');
        assert!(
            a.cfg.repos.favorites.is_empty(),
            "favorite must not stick when the save is refused"
        );
        assert!(a.status.contains("not saving"), "{}", a.status);
        press(&mut a, 'H');
        assert!(a.cfg.repos.hidden.is_empty());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "repos = [broken\n",
            "their file is untouched"
        );
        // a good config saves and keeps the change
        std::fs::write(&path, "").unwrap();
        press(&mut a, 'f');
        assert!(
            a.cfg.repos.is_fav("o/a") && std::fs::read_to_string(&path).unwrap().contains("o/a")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn filter_matches_numbers_and_notifications_drop_hidden_repos() {
        let mut a = App::with(
            "o/r".into(),
            Theme::new(false, IconSet::Unicode, true),
            false,
        );
        let i = a.panel_idx(PK::Prs).unwrap();
        let it = |n, t: &str| Item {
            number: n,
            title: t.into(),
            kind: Kind::Pr,
            ..Default::default()
        };
        a.panels[i].items = vec![it(14320, "rate limits"), it(7, "docs")];
        a.filter = "#143".into();
        assert_eq!(a.visible(i).len(), 1);
        a.filter = "7".into();
        assert_eq!(
            a.visible(i).iter().map(|x| x.number).collect::<Vec<_>>(),
            [7]
        );
        a.filter = "docs".into();
        assert_eq!(a.visible(i).len(), 1);
    }
}
