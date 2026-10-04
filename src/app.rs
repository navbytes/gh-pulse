use crate::act::{self, Action, FormKind, Sel};
use crate::browse::{self, Browser, Out, RepoRow};
use crate::config::{self, Act, Config, Keymap, PanelName, TabDef};
use crate::dcache;
use crate::diff::{self, DiffMode};
use crate::form::{self, Form};
use crate::gh::{self, Data, FilesData, Item, Kind, Tab};
use crate::global::{self, Query, Scope, Section};
use crate::pool::{self, Prio};
use crate::rate::{self, RateState};
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
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;

/// Every list load gets a number nobody else has had: panels of different homes (a repo, then
/// another repo, the global home) can't mistake one another's late replies for their own.
static SEQ: AtomicU64 = AtomicU64::new(0);

fn next_seq() -> u64 {
    SEQ.fetch_add(1, Relaxed) + 1
}

/// A repo's header facts as cached: with the host and login they were fetched as, so another
/// account (or host) never sees them.
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedMeta {
    host: String,
    viewer: String,
    meta: gh::RepoMeta,
}

fn meta_key(host: &str, repo: &str) -> String {
    format!("meta-{host}-{repo}")
}

/// Cached facts for `repo` if fresh and stored for this host and login (None when the login is
/// unknown: nothing user-specific is cached then).
fn read_meta(repo: &str, ttl: u64) -> Option<gh::RepoMeta> {
    let (host, login) = gh::identity()?;
    let st = crate::cache::Store::default_if_enabled()?;
    let c: CachedMeta = st.fresh(&meta_key(&host, repo), ttl, rate::now())?;
    let mut m = c.meta;
    (c.host == host && c.viewer.eq_ignore_ascii_case(&login)).then(|| {
        m.clean();
        m
    })
}

/// `viewer` is who the answer says we are; it must match the login the entry is stored under.
fn write_meta(repo: &str, meta: &gh::RepoMeta, viewer: &str) {
    let (Some((host, login)), Some(st)) =
        (gh::identity(), crate::cache::Store::default_if_enabled())
    else {
        return;
    };
    if viewer.eq_ignore_ascii_case(&login) {
        let c = CachedMeta {
            host: host.clone(),
            viewer: login,
            meta: meta.clone(),
        };
        let _ = st.write(&meta_key(&host, repo), &c, rate::now());
    }
}

/// Panel identity. Files / Checks / Comments are derived from the selected (or drilled-into) PR.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PK {
    Status,
    Prs,
    Files,
    Commits,
    Checks,
    Comments,
    Issues,
    Actions,
    /// Branches / Tags / Releases as list tabs.
    Repo,
    Notifs,
    // global-home sections (searches across repos)
    Review,
    MyPrs,
    Assigned,
    Involved,
    /// Favorites / Recent repos.
    Repos,
    /// `[[sections]]` entry by index: a custom search of pull requests / of issues.
    CustomPr(u8),
    CustomIssue(u8),
}

impl PK {
    /// Index into `[[sections]]`.
    pub fn custom(self) -> Option<usize> {
        match self {
            PK::CustomPr(i) | PK::CustomIssue(i) => Some(usize::from(i)),
            _ => None,
        }
    }

    pub fn derived(self) -> bool {
        matches!(self, PK::Files | PK::Commits | PK::Checks | PK::Comments)
    }

    /// Lists whose rows are pull requests (the Files panel follows the one you were last in).
    pub fn is_pr_list(self) -> bool {
        matches!(
            self,
            PK::Prs | PK::Review | PK::MyPrs | PK::Involved | PK::CustomPr(_)
        )
    }

    /// The global sections: a GitHub search each, loaded when first focused, never counted in the background.
    pub fn is_global_search(self) -> bool {
        matches!(self, PK::Review | PK::MyPrs | PK::Assigned | PK::Involved)
            || self.custom().is_some()
    }
}

/// The home that is not on screen (repo or global) with what it needs to come back as it was:
/// cursor, tab, filter, and for a repo its name and facts. Empty `panels` means "not built yet".
struct Side {
    panels: Vec<Panel>,
    focus: usize,
    repo: String,
    meta: Option<gh::RepoMeta>,
    counts: HashMap<CountKey, (usize, bool)>,
    branch: String,
    cwd_branch: Option<String>,
    filter: String,
    pr_src: usize,
    from_global: bool,
}

impl Side {
    fn empty(repo: String) -> Side {
        Side {
            panels: vec![],
            focus: 0,
            repo,
            meta: None,
            counts: HashMap::new(),
            branch: String::new(),
            cwd_branch: None,
            filter: String::new(),
            pr_src: 0,
            from_global: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Loc {
    Active,
    Ctx,
    Other,
}

/// What choosing a row in the scope picker does.
#[derive(Clone, Debug, PartialEq)]
pub enum ScopeChoice {
    Set(Scope),
    Stage(ScopeStage),
}

/// The `s` picker for the global home's scope.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScopeStage {
    Top,
    Orgs,
    Repos,
}

pub struct ScopePicker {
    pub stage: ScopeStage,
    pub cursor: usize,
    /// Type-ahead in the Orgs and Repos stages.
    pub query: String,
    /// Repo names the Repos stage filters (favorites, recent, the cached repo list): read once on
    /// entering the stage, not on every frame and key.
    pub names: Vec<String>,
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
    pub tabs: Vec<TabDef>,
    /// Index into `tabs`.
    pub tab: usize,
    pub items: Vec<Item>,
    pub loading: bool,
    pub error: Option<String>,
    pub cursor: usize,
    seq: u64,
    /// Mirrors `seq` for queued jobs: a job whose number is no longer current never starts.
    stamp: Arc<AtomicU64>,
    /// Not loaded yet: the REST-backed panels wait for their first focus (saves startup calls).
    pub unloaded: bool,
    /// Rows left out because their repo is hidden (global sections).
    pub hidden: usize,
    /// Favorites a favorites-scoped search could not include.
    pub not_shown: usize,
    /// The rows are a copy from disk (fetched at this time) while a refresh is on its way.
    pub cached_at: Option<u64>,
}

impl Panel {
    fn new(kind: PK, title: &'static str, tabs: Vec<TabDef>) -> Self {
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
            stamp: Arc::new(AtomicU64::new(0)),
            unloaded: false,
            hidden: 0,
            not_shown: 0,
            cached_at: None,
        }
    }

    /// Searches (GitHub's search API / GraphQL search) are the tightest-limited lists: their counts
    /// are never fetched in the background.
    fn search_backed(kind: PK, id: usize) -> bool {
        kind.is_global_search() || matches!((kind, id), (PK::Prs | PK::Issues, 0 | 1))
    }

    /// The `gh::list` id of the active tab.
    pub fn tab_id(&self) -> usize {
        self.tabs.get(self.tab).map_or(0, |t| t.id)
    }

    /// (source panel, tab) understood by `gh::list`; None for derived panels.
    pub fn source(&self) -> Option<(usize, usize)> {
        source_of(self.kind, self.tab_id())
    }

    /// Switch to the tab with this `gh::list` id; false when it is not configured.
    fn set_tab_id(&mut self, id: usize) -> bool {
        match self.tabs.iter().position(|t| t.id == id) {
            Some(i) => {
                self.tab = i;
                true
            }
            None => false,
        }
    }
}

fn source_of(kind: PK, id: usize) -> Option<(usize, usize)> {
    Some(match kind {
        PK::Status => (0, 0),
        PK::Prs => (1, id),
        PK::Issues => (2, id),
        PK::Actions => (3, id),
        PK::Repo => (
            match id {
                0 => 4,
                1 => 7,
                _ => 5,
            },
            0,
        ),
        PK::Notifs => (6, 0),
        _ => return None,
    })
}

/// `source()` ids that make a panel's tab count meaningful.
type CountKey = (PK, usize);

/// The unread-notifications view (`N`).
pub struct Inbox {
    pub items: Vec<Item>,
    pub cursor: usize,
    pub loading: bool,
    pub error: Option<String>,
}

// A cache entry is built once per fetch; boxing the comment data would only add indirection.
#[allow(clippy::large_enum_variant)]
/// What is known about a cached detail: the item's `updatedAt` and the time it was fetched, and
/// where it lives on disk.
#[derive(Clone, Default)]
struct DMeta {
    stamp: String,
    at: u64,
    /// After a failed refresh: no new attempt before this time.
    hold: u64,
    repo: String,
    kind: Kind,
    number: u64,
}

// one entry per cached detail: a few hundred bytes each is cheaper than boxing every read
#[allow(clippy::large_enum_variant)]
pub enum Load {
    Loading,
    Done(Result<Data, String>),
}

/// What to do after a confirmed command succeeds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Then {
    None,
    /// Select the item the command created (its number is the URL's last segment).
    Select(PK),
}

pub struct Confirm {
    pub cmd: Vec<String>,
    /// Piped to the command's stdin (large bodies); the popup says so.
    pub stdin: Option<String>,
    pub then: Then,
    /// The form this came from; n/Esc reopens it with its content.
    back: Option<Box<Form>>,
    local_of: Option<String>,
    pub scroll: u16,
    /// Set by the renderer so scrolling can't overshoot.
    pub max_scroll: Cell<u16>,
    /// Rows visible in the popup (set by the renderer), for paging.
    pub view_h: Cell<u16>,
}

impl Confirm {
    #[cfg(test)]
    pub fn new_for_test(cmd: Vec<String>) -> Self {
        Self::new(cmd, None)
    }

    fn new(cmd: Vec<String>, local_of: Option<String>) -> Self {
        Confirm {
            cmd,
            stdin: None,
            then: Then::None,
            back: None,
            local_of,
            scroll: 0,
            max_scroll: Cell::new(0),
            view_h: Cell::new(10),
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
    Form(Box<Form>),
    Scope(ScopePicker),
}

/// How the global lists are sectioned (`g` cycles): flat, by PR author, by repo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupBy {
    None,
    Author,
    Repo,
}

impl GroupBy {
    pub fn next(self) -> GroupBy {
        match self {
            GroupBy::None => GroupBy::Author,
            GroupBy::Author => GroupBy::Repo,
            GroupBy::Repo => GroupBy::None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GroupBy::None => "none",
            GroupBy::Author => "author",
            GroupBy::Repo => "repo",
        }
    }

    /// The heading an item falls under; empty when not grouped.
    pub fn key(self, it: &Item) -> String {
        match self {
            GroupBy::None => String::new(),
            GroupBy::Author => it.author.login.clone(),
            GroupBy::Repo => it.repo.clone(),
        }
    }
}

/// Screen regions recorded by the last draw, for mouse hit-testing.
#[derive(Default, Clone)]
pub struct Hit {
    pub panels: Vec<Rect>,
    pub list: Rect,
    pub list_off: usize,
    /// Grouped lists: the item behind each list row (None for a heading). Empty when rows are items.
    pub list_rows: Vec<Option<usize>>,
    pub detail: Rect,
    pub body: Rect,
    pub tab_y: u16,
    pub tabs: Vec<(u16, u16)>,
    /// Clickable list-tab labels in the panel titles.
    pub ptabs: Vec<PTab>,
}

/// One list-tab label in a panel title: screen row, columns [x0, x1), panel and tab index.
#[derive(Clone, Copy)]
pub struct PTab {
    pub y: u16,
    pub x0: u16,
    pub x1: u16,
    pub panel: usize,
    pub tab: usize,
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
    /// Replace the rows instead of appending (cached rows being swapped for fresh ones).
    replace: bool,
}

// How many times the repo-list cache was read (tests assert the picker does not re-read per key).
#[cfg(test)]
thread_local! {
    static READS: Cell<usize> = const { Cell::new(0) };
}

/// The repo list as cached, with who it was fetched as.
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedRepos {
    host: String,
    viewer: String,
    rows: Vec<RepoRow>,
    truncated: bool,
}

fn repos_key(host: &str, login: &str) -> String {
    format!("repos-{host}-{login}")
}

/// Cached rows and their age, for this host and login only (and re-cleaned: the disk is not trusted).
fn read_repos() -> Option<(CachedRepos, u64)> {
    #[cfg(test)]
    READS.with(|r| r.set(r.get() + 1));
    let (host, login) = gh::identity()?;
    let st = crate::cache::Store::default_if_enabled()?;
    let (mut c, age): (CachedRepos, u64) = st.read(&repos_key(&host, &login), rate::now())?;
    if c.host != host || !c.viewer.eq_ignore_ascii_case(&login) {
        return None;
    }
    use crate::sanitize::clean_in_place as cl;
    for r in &mut c.rows {
        [&mut r.name, &mut r.owner, &mut r.lang, &mut r.pushed]
            .into_iter()
            .for_each(cl);
    }
    Some((c, age))
}

/// One listing in progress: a page per pool job, so a long account never holds a worker.
struct BrowseJob {
    tx: Sender<Msg>,
    seq: u64,
    seq_a: Arc<AtomicU64>,
    /// Cached rows are already on screen: the new listing lands in one piece at the end.
    cached: bool,
    all: Vec<RepoRow>,
    total: usize,
}

fn browse_step(j: BrowseJob, after: Option<String>) {
    let (seq_a, seq, tx) = (j.seq_a.clone(), j.seq, j.tx.clone());
    pool::global().submit(
        Prio::User,
        move || seq_a.load(Relaxed) != seq,
        move || browse_run(j, after),
        move || {
            let _ = tx.send(Msg::Repos(seq, Err("could not load the repo list".into())));
        },
    );
}

fn browse_run(mut j: BrowseJob, after: Option<String>) {
    let p = match browse::fetch_page(after.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            let _ = j.tx.send(Msg::Repos(j.seq, Err(e)));
            return;
        }
    };
    j.total += p.rows.len();
    let capped = p.next.is_some() && j.total >= browse::MAX_REPOS;
    let last = p.next.is_none() || capped;
    let next = p.next.clone();
    j.all.extend(p.rows.iter().cloned());
    if last
        && let (Some((host, login)), Some(st)) =
            (gh::identity(), crate::cache::Store::default_if_enabled())
        && p.viewer.eq_ignore_ascii_case(&login)
    {
        let c = CachedRepos {
            host: host.clone(),
            viewer: login.clone(),
            rows: j.all.clone(),
            truncated: capped,
        };
        let _ = st.write(&repos_key(&host, &login), &c, rate::now());
    }
    let m = if j.cached {
        last.then(|| RepoMsg {
            viewer: p.viewer.clone(),
            rows: j.all.clone(),
            last,
            truncated: capped,
            replace: true,
        })
    } else {
        Some(RepoMsg {
            viewer: p.viewer,
            rows: p.rows,
            last,
            truncated: capped,
            replace: false,
        })
    };
    if let Some(m) = m
        && j.tx.send(Msg::Repos(j.seq, Ok(m))).is_err()
    {
        return;
    }
    if !last {
        browse_step(j, next);
    }
}

enum Msg {
    // Results carry the generation they were requested in; stale ones are dropped.
    List(PK, usize, u64, Result<Vec<Item>, String>),
    Detail(String, Tab, u64, Result<Data, String>),
    /// When the detail that follows was fetched (and for which `updatedAt`).
    DStamp(String, Tab, u64, DMeta),
    /// A copy from disk that is past its time: shown now, while a refresh is on its way.
    DStale(String, Tab, u64, Data, DMeta),
    /// A later page of comments for the item with this key.
    More(String, u64, gh::More),
    Log(String, String, u64, Result<String, String>),
    Status(Result<String, String>),
    Done(Result<String, String>, Then),
    FormData(u64, Box<gh::FormData>),
    /// The branch checked out in the working directory, when it is a clone of the repo.
    Branch(String, Option<String>),
    Repos(u64, Result<RepoMsg, String>),
    /// Repo facts for the header (generation of the header lookup).
    Meta(String, gh::RepoMeta),
    /// (list generation, panel, tab id, (rows, more)) for the other tabs' counts.
    Count(u64, CountKey, Result<(usize, bool), String>),
    /// Unread notifications, for the badge (and the inbox when open).
    Inbox(u64, Result<Vec<Item>, String>),
    /// The viewer's login (from the startup request).
    User(u64, String),
    /// A queued detail fetch that was dropped before it started: forget its placeholder.
    Dropped(String, Tab, u64),
    /// The batched startup request failed (not a rate limit): load the panels one by one instead.
    StartupFallback(String),
    /// A global section's side facts, sent just before its list: (panel, seq, hidden rows, favorites left out).
    GMeta(PK, usize, u64, usize, usize, Option<String>),
    /// Your organizations, for the scope picker.
    Orgs(Result<Vec<String>, String>),
    /// Who reacted to one comment: (item key, comment id, names or the error).
    Reactors(String, String, u64, Result<Vec<gh::Reaction>, String>),
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
    /// Checked-out branch of the cwd clone ("" when it isn't one) and the viewer, for `header`.
    branch: String,
    user: String,
    pub meta: Option<gh::RepoMeta>,
    /// The shared quota state as of the last `poll`, and what follows from it.
    pub rate: RateState,
    /// Header chip: the quota is low (and when it comes back, as HH:MM).
    pub quota_low: bool,
    pub paused: bool,
    pub rate_clock: Option<(u64, String)>,
    rate_at: Option<std::time::Instant>,
    /// The last key or mouse event, for settle delays and the lazy counts.
    last_input: std::time::Instant,
    /// Detail fetches the screen still wants, so queued ones for items we moved off are dropped.
    wanted: Arc<Mutex<HashSet<(String, Tab)>>>,
    want_now: HashSet<(String, Tab)>,
    /// What the previous idle tick wanted (the Overview debounce).
    want_prev: HashSet<(String, Tab)>,
    /// Mirrors of the generations, readable from queued jobs.
    dgen_a: Arc<AtomicU64>,
    cgen_a: Arc<AtomicU64>,
    pub panels: Vec<Panel>,
    /// Row counts of every configured list tab, as they become known.
    pub counts: HashMap<CountKey, (usize, bool)>,
    cpending: HashSet<CountKey>,
    /// Count lookups that failed (timeout, error): not retried until the next reload.
    pub count_failed: HashSet<CountKey>,
    /// Bumped when the lists are reloaded, so late counts for the old ones are dropped.
    cgen: u64,
    pub inbox: Option<Inbox>,
    /// Repos of the unread notifications once fetched (None: unknown or the fetch failed).
    unread: Option<Vec<String>>,
    inbox_seq: u64,
    unread_at: Option<std::time::Instant>,
    /// Comments whose reactor names are being fetched.
    react_pending: HashSet<String>,
    pub focus: usize,
    pub filter: String,
    pub typing: bool,
    pub help: bool,
    /// The `?` menu: cursor over the visible rows, its filter, whether `/` is typing, first line shown.
    pub help_scroll: usize,
    pub help_filter: String,
    /// How far back the global PR sections look (`W`), and how global lists are grouped (`g`).
    pub window: config::Window,
    pub group: GroupBy,
    pub help_typing: bool,
    pub help_top: Cell<usize>,
    /// Set by the renderer: how far the help popup can scroll.
    pub help_max: Cell<usize>,
    /// The global home is on screen (otherwise a repo).
    pub global: bool,
    /// The other home, parked.
    other: Option<Side>,
    /// What the global sections are narrowed to.
    pub scope: Scope,
    /// Reached the repo from the global home: the header says so and `G` goes back.
    pub from_global: bool,
    /// Where the scope and the recent repos are persisted (None in tests).
    state_dir: Option<PathBuf>,
    recent: crate::state::Recent,
    /// Your organizations once fetched (scope picker).
    pub orgs: Option<Vec<String>>,
    /// The PR list the Files panel follows (the one you were last in).
    pr_src: usize,
    pub show_log: bool,
    pub modal: Option<Modal>,
    pub status: String,
    pub detail_focus: bool,
    dtab: usize,
    pub row: usize,
    pub ctx: Option<Ctx>,
    pub zoom: bool,
    form_seq: u64,
    /// False in layout tests: nothing may start a `gh` process.
    net: bool,
    cwd_branch: Option<String>,
    /// Bumped per header lookup so a late answer for the previous repo is dropped.
    hgen: u64,
    /// (panel, number, repo) of an item just created; honoured by the next list reload only.
    pending_select: Option<(PK, u64, String)>,
    /// File shown from the selected commit (Commits panel).
    pub cfile: usize,
    /// The file shown in the Diff tab when `files` is not among the panels.
    file0: usize,
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
    /// How old each revalidated detail is; entries without one are never refreshed on their own.
    dmeta: HashMap<(String, Tab), DMeta>,
    /// Details being refreshed while their old copy stays on screen.
    revalidating: HashSet<(String, Tab)>,
    /// Disk entries stored before this are ignored (`R`), or before the item's own time (`r`).
    disk_floor: u64,
    item_floor: HashMap<String, u64>,
    /// Bumped whenever `cache` is cleared or the repo scope changes.
    dgen: u64,
    repos_seq: u64,
    repos_a: Arc<AtomicU64>,
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

fn build_panels(cfg: &Config) -> Vec<Panel> {
    panels_from(cfg.panels.layout(&cfg.sections), &cfg.sections)
}

fn build_global_panels(cfg: &Config) -> Vec<Panel> {
    panels_from(cfg.panels.global_layout(&cfg.sections), &cfg.sections)
}

/// Panel titles are `&'static str`; a section's is leaked once per distinct text (at most a dozen).
fn leak_title(s: String) -> &'static str {
    static SEEN: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = seen.iter().find(|t| **t == s) {
        return t;
    }
    let t: &'static str = Box::leak(s.into_boxed_str());
    seen.push(t);
    t
}

fn panels_from(specs: Vec<config::PanelSpec>, secs: &[config::SectionCfg]) -> Vec<Panel> {
    specs
        .into_iter()
        .filter_map(|s| {
            let (kind, title) = match &s.name {
                PanelName::Prs => (PK::Prs, "Pull requests"),
                PanelName::Files => (PK::Files, "Files"),
                PanelName::Issues => (PK::Issues, "Issues"),
                PanelName::Actions => (PK::Actions, "Actions"),
                // titled by its tabs: "Branches 32 \u{b7} Tags 14 \u{b7} Releases 81"
                PanelName::Repo => (PK::Repo, ""),
                PanelName::Notifications => (PK::Notifs, "Notifications"),
                PanelName::Status => (PK::Status, "Status"),
                PanelName::Repos => (PK::Repos, "Repos"),
                PanelName::Review => (PK::Review, "Review requested"),
                PanelName::Mine => (PK::MyPrs, "My PRs"),
                PanelName::Assigned => (PK::Assigned, "Issues"),
                PanelName::Involved => (PK::Involved, "Involved"),
                PanelName::Section(t) => {
                    let i = secs.iter().position(|c| c.title == *t)?;
                    let c = &secs[i];
                    let own = config::parse_filter(&c.filter).is_ok_and(|f| f.own_scope);
                    let n = u8::try_from(i).ok()?;
                    let name = crate::sanitize::clean(&c.title);
                    (
                        match c.kind {
                            config::SectionKind::Prs => PK::CustomPr(n),
                            config::SectionKind::Issues => PK::CustomIssue(n),
                        },
                        leak_title(if own {
                            format!("{name} (own filter)")
                        } else {
                            name.into_owned()
                        }),
                    )
                }
            };
            let mut p = Panel::new(kind, title, s.tabs);
            p.tab = s.tab;
            Some(p)
        })
        .collect()
}

/// Does the `s` scope narrow this section (it has no `repo:` / `org:` / `user:` of its own)?
fn scope_applies(c: Option<&config::SectionCfg>) -> bool {
    c.is_none_or(|c| config::parse_filter(&c.filter).is_ok_and(|f| !f.own_scope))
}

fn step(cur: usize, d: isize, max: usize) -> usize {
    cur.min(max).saturating_add_signed(d).min(max)
}

impl App {
    /// The real entry point: config and keymap come from the user's config file.
    pub fn from_config(
        repo: Option<String>,
        global: bool,
        theme: Theme,
        cfg: Config,
        keys: Keymap,
    ) -> Self {
        gh::set_sync_viewed(cfg.sync_viewed);
        let mut app = Self::build_start(repo, global, theme, true, cfg);
        (app.keys, app.cfg_path) = (keys, config::path());
        let (viewed, warn) = Viewed::load(crate::state::path());
        app.viewed = viewed;
        if let Some(w) = warn.or_else(|| config::warnings(&app.cfg).into_iter().next()) {
            app.status = w;
        }
        app
    }

    /// `load: false` skips all `gh` calls (layout tests).
    #[cfg(test)]
    pub fn with(repo: String, theme: Theme, load: bool) -> Self {
        Self::build(repo, theme, load, Config::default())
    }

    #[cfg(test)]
    pub fn build(repo: String, theme: Theme, load: bool, cfg: Config) -> Self {
        Self::build_start(Some(repo), false, theme, load, cfg)
    }

    /// `repo`: the repo context (the one shown, or the one `G` leads to); `global`: open the home.
    pub fn build_start(
        repo: Option<String>,
        global: bool,
        theme: Theme,
        load: bool,
        cfg: Config,
    ) -> Self {
        let (tx, rx) = channel();
        let panels = if global {
            build_global_panels(&cfg)
        } else {
            build_panels(&cfg)
        };
        let repo_name = repo.clone().unwrap_or_default();
        let window = cfg.ui.window;
        let mut app = App {
            cfg,
            cfg_path: None,
            keys: Keymap::build(&Default::default()).expect("default keys are valid"),
            browser: None,
            theme,
            hit: RefCell::default(),
            tick: 0,
            hl: RefCell::new(None),
            header: if global {
                " all repos".to_string()
            } else {
                format!(" {repo_name}")
            },
            repo: if global {
                String::new()
            } else {
                repo_name.clone()
            },
            meta: None,
            panels,
            counts: HashMap::new(),
            cpending: HashSet::new(),
            count_failed: HashSet::new(),
            cgen: 0,
            cgen_a: Arc::new(AtomicU64::new(0)),
            dgen_a: Arc::new(AtomicU64::new(0)),
            wanted: Arc::default(),
            want_now: HashSet::new(),
            want_prev: HashSet::new(),
            last_input: std::time::Instant::now(),
            rate: RateState::default(),
            quota_low: false,
            paused: false,
            rate_clock: None,
            rate_at: None,
            branch: String::new(),
            user: String::new(),
            inbox: None,
            unread: None,
            inbox_seq: 0,
            unread_at: None,
            react_pending: HashSet::new(),
            focus: 0,
            filter: String::new(),
            typing: false,
            help: false,
            help_scroll: 0,
            help_filter: String::new(),
            window,
            group: GroupBy::None,
            help_typing: false,
            help_top: Cell::new(0),
            help_max: Cell::new(0),
            global,
            other: (global && repo.is_some()).then(|| Side::empty(repo_name)),
            scope: Scope::All,
            from_global: false,
            state_dir: None,
            recent: crate::state::Recent::new(&gh::host()),
            orgs: None,
            pr_src: 0,
            show_log: false,
            modal: None,
            status: String::new(),
            detail_focus: false,
            dtab: 0,
            row: 0,
            ctx: None,
            zoom: false,
            form_seq: 0,
            net: load,
            cwd_branch: None,
            hgen: 0,
            pending_select: None,
            cfile: 0,
            file0: 0,
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
            dmeta: HashMap::new(),
            revalidating: HashSet::new(),
            disk_floor: 0,
            item_floor: HashMap::new(),
            dgen: 0,
            repos_seq: 0,
            repos_a: Arc::new(AtomicU64::new(0)),
            tx,
            rx,
        };
        app.pr_src = app
            .panels
            .iter()
            .position(|p| p.kind.is_pr_list())
            .unwrap_or(0);
        if global {
            app.rebuild_header();
        }
        if load {
            rate::set_limits(app.cfg.api.low_quota_percent, app.cfg.api.pause_percent);
            crate::cache::set_enabled(app.cfg.api.cache);
            crate::cache::set_slow(app.cfg.cache.slow_s);
            // old and surplus cached details and lists go, off the UI thread
            std::thread::spawn(dcache::prune);
            pool::init(app.cfg.api.max_concurrent);
            gh::set_timeout(app.cfg.api.timeout_s);
            if let Some(d) = crate::state::dir() {
                app.scope = crate::state::load_scope(&d.join("scope.json"), &gh::host())
                    .and_then(|s| Scope::parse(&s))
                    .unwrap_or(Scope::All);
                app.recent = crate::state::Recent::load(&d.join("recent.json"), &gh::host());
                // an existing directory of ours is tightened to 0700 now; a foreign or linked one is refused
                match crate::cache::ensure_dir(&d) {
                    Ok(()) => app.state_dir = Some(d),
                    Err(e) => app.status = format!("state not saved: {e}"),
                }
            }
            if app.global {
                app.load_global_side();
            } else {
                app.load_repo_side();
            }
            app.spawn_unread(Prio::Background);
            app.spawn_rate_poll();
        }
        app
    }

    /// The repo home's first load: PRs and Issues in one request, the rest when first focused.
    fn load_repo_side(&mut self) {
        // REST-backed panels load when first focused; the focused one (and PRs/Issues) now
        for p in &mut self.panels {
            p.unloaded = !matches!(p.kind, PK::Prs | PK::Issues | PK::Files | PK::Repos);
        }
        let f = self.focus;
        self.panels[f].unloaded = false;
        self.spawn_header();
        self.start_load(false);
        self.load_unfocused_lazy_marks();
        self.touch_recent();
        for i in 0..self.panels.len() {
            if self.panels[i].kind == PK::Repos {
                self.load_panel(i, false);
            }
        }
    }

    /// The global home's first load: only the focused section searches; the others wait for focus.
    fn load_global_side(&mut self) {
        for p in &mut self.panels {
            p.unloaded = true;
        }
        let f = self.focus;
        self.panels[f].unloaded = false;
        // who you are comes from gh's own config (no API call) when it is recorded there
        if let Some((_, login)) = gh::identity() {
            self.user = login;
        } else if self.net {
            let (tx, g) = (self.tx.clone(), self.hgen);
            self.user_job(move || {
                let _ = tx.send(Msg::User(g, gh::user()));
            });
        }
        self.rebuild_header();
        for i in 0..self.panels.len() {
            if i == f || self.panels[i].kind == PK::Repos {
                self.load_panel(i, false);
            }
        }
    }

    /// Panels the user hasn't looked at keep their placeholder; the focused one loads now.
    fn load_unfocused_lazy_marks(&mut self) {
        for i in 0..self.panels.len() {
            if !self.panels[i].unloaded {
                // PRs/Issues came with the startup request; REST panels that are loaded load here
                if !matches!(self.panels[i].kind, PK::Prs | PK::Issues | PK::Files) {
                    self.load_panel(i, false);
                }
            }
        }
    }

    /// Queue a job: `stale` is asked when it reaches the front, `dropped` runs instead if it is.
    fn queue(
        &self,
        prio: Prio,
        stale: impl Fn() -> bool + Send + 'static,
        run: impl FnOnce() + Send + 'static,
        dropped: impl FnOnce() + Send + 'static,
    ) {
        if self.net {
            pool::global().submit(prio, stale, run, dropped);
        }
    }

    /// Work the user asked for. If it can't run (queue full, the job panicked) the status line says so.
    fn user_job(&self, run: impl FnOnce() + Send + 'static) {
        let tx = self.tx.clone();
        self.user_job_or(run, move || {
            let _ = tx.send(Msg::Status(Err(
                "a request could not be run; try again".into()
            )));
        });
    }

    /// Like `user_job`, with the caller's own way of hearing that the job was lost.
    fn user_job_or(
        &self,
        run: impl FnOnce() + Send + 'static,
        lost: impl FnOnce() + Send + 'static,
    ) {
        self.queue(Prio::User, || false, run, lost);
    }

    /// A write to GitHub (a confirmed command, a viewed mark). It never waits in the pool behind reads
    /// or a hung `gh` and is never dropped for want of room: it gets a thread of its own. `Msg::Done` /
    /// `Status` carry the result; a panic is reported as a failure.
    fn mutation_job(&self, run: impl FnOnce() + Send + 'static, failed: Msg) {
        if !self.net {
            return;
        }
        let tx = self.tx.clone();
        thread::spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).is_err() {
                let _ = tx.send(failed);
            }
        });
    }

    /// Local part of the header (the cwd's branch, when the directory is a clone of this repo).
    fn spawn_header(&mut self) {
        self.hgen += 1;
        let (tx, repo) = (self.tx.clone(), self.repo.clone());
        self.user_job(move || {
            let branch = gh::cwd_is_clone_of(&repo)
                .then(|| {
                    Command::new("git")
                        .args(["branch", "--show-current"])
                        .output()
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                        .unwrap_or_default()
                })
                .filter(|b| !b.is_empty());
            let _ = tx.send(Msg::Branch(repo, branch));
        });
    }

    fn rebuild_header(&mut self) {
        let user = if self.user.is_empty() {
            String::new()
        } else {
            format!("  user: {}", self.user)
        };
        self.header = if self.global {
            let group = match self.group {
                GroupBy::None => String::new(),
                g => format!("  group: {}", g.label()),
            };
            format!(
                " all repos  scope: {}  PRs: {}{group}{user}",
                self.scope.label(),
                self.window.label()
            )
        } else if self.from_global {
            format!(
                " all repos \u{203a} {}  branch: {}{user}",
                self.repo, self.branch
            )
        } else {
            format!(" {}  branch: {}{user}", self.repo, self.branch)
        };
    }

    /// Remember the repo you are in (Recent, newest first), persisted next to the other state.
    fn touch_recent(&mut self) {
        if self.repo.is_empty() {
            return;
        }
        self.recent.touch(&self.repo.clone());
        if let Some(d) = &self.state_dir
            && let Err(e) = self.recent.save(&d.join("recent.json"))
        {
            self.status = format!("recent repos not saved: {e}");
        }
    }

    /// One GraphQL request for the PR and Issues lists, the viewer and the repo facts (cached for
    /// five minutes); everything else waits for its first focus. `fresh` bypasses the cached facts.
    fn start_load(&mut self, fresh: bool) {
        let cov: Vec<usize> = (0..self.panels.len())
            .filter(|&i| matches!(self.panels[i].kind, PK::Prs | PK::Issues) && !self.global)
            .collect();
        let mut want = gh::StartupWant {
            meta: true,
            ..Default::default()
        };
        if !fresh
            && self.net
            && let Some(m) = read_meta(&self.repo, self.cfg.cache.slow_s)
        {
            (self.meta, want.meta) = (Some(m), false);
        }
        let mut jobs = vec![];
        for &i in &cov {
            let (kind, tab_id) = (self.panels[i].kind, self.panels[i].tab_id());
            let key = self.list_key(kind, tab_id);
            // a copy kept on disk is shown at once; while it is fresh this list needs no request
            if self.net && !fresh && self.show_cached_list(i, key.as_deref()) {
                continue;
            }
            let p = &mut self.panels[i];
            p.loading = true;
            p.error = None;
            p.seq = next_seq();
            p.unloaded = false;
            p.stamp.store(p.seq, Relaxed);
            let tab = p.tab_id();
            match p.kind {
                PK::Prs => want.prs = Some(tab),
                _ => want.issues = Some(tab),
            }
            jobs.push((p.kind, p.tab, p.seq, p.stamp.clone(), key));
        }
        if !self.net {
            return;
        }
        if cov.is_empty() {
            // global view or no PR/issue panel: only the facts (and the viewer) are needed
            want.meta = want.meta || self.meta.is_none();
        } else if jobs.is_empty()
            && !want.meta
            && let Some((_, login)) = gh::identity()
        {
            // every list and the facts came from fresh copies on disk, and gh's own config names
            // the viewer: nothing to ask GitHub
            if self.user.is_empty() {
                self.user = login;
                self.rebuild_header();
            }
            return;
        }
        let (tx, repo, g, cg) = (self.tx.clone(), self.repo.clone(), self.hgen, self.cgen);
        let (tx_lost, lost): (_, Vec<_>) = (
            self.tx.clone(),
            jobs.iter().map(|(k, t, sq, _, _)| (*k, *t, *sq)).collect(),
        );
        self.user_job_or(
            move || match gh::startup(&repo, &want) {
                Ok(st) => {
                    for (kind, tab, seq, stamp, key) in jobs {
                        if stamp.load(Relaxed) != seq {
                            continue;
                        }
                        let items = if kind == PK::Prs { &st.prs } else { &st.issues };
                        if let (Some(k), Some(v)) = (&key, items) {
                            dcache::write_list(k, v, 0, 0, rate::now());
                        }
                        let _ = tx.send(Msg::List(
                            kind,
                            tab,
                            seq,
                            Ok(items.clone().unwrap_or_default()),
                        ));
                    }
                    if let Some(m) = st.meta {
                        write_meta(&repo, &m, &st.user);
                        let _ = tx.send(Msg::Meta(repo.clone(), m));
                    }
                    let _ = tx.send(Msg::User(g, st.user));
                    if let Some(n) = st.open_prs {
                        let _ = tx.send(Msg::Count(cg, (PK::Prs, 2), Ok((n, false))));
                    }
                }
                Err(e) if gh::rate_limit_secs(&e).is_some() => {
                    for (kind, tab, seq, _, _) in jobs {
                        let _ = tx.send(Msg::List(kind, tab, seq, Err(e.clone())));
                    }
                }
                Err(_) => {
                    let _ = tx.send(Msg::StartupFallback(repo));
                }
            },
            // lost (queue full, panic): the lists must not sit on "loading..."
            move || {
                for (kind, tab, seq) in lost {
                    let _ = tx_lost.send(Msg::List(
                        kind,
                        tab,
                        seq,
                        Err("could not load; try again (R)".into()),
                    ));
                }
            },
        );
    }

    /// The batched request failed: the old way, one `gh` command per panel, plus the facts.
    fn fallback_start(&mut self) {
        for i in 0..self.panels.len() {
            if matches!(self.panels[i].kind, PK::Prs | PK::Issues) && self.panels[i].loading {
                self.load_panel(i, false);
            }
        }
        let (tx, repo, g) = (self.tx.clone(), self.repo.clone(), self.hgen);
        self.user_job(move || {
            let _ = tx.send(Msg::User(g, gh::user()));
            if let Ok(m) = gh::repo_meta(&repo) {
                let _ = tx.send(Msg::Meta(repo, m));
            }
        });
    }

    /// Unread notifications for the header badge; the badge stays hidden when the fetch fails.
    fn spawn_unread(&mut self, prio: Prio) {
        if !self.net {
            return;
        }
        self.unread_at = Some(std::time::Instant::now());
        self.inbox_seq += 1;
        let (tx, seq) = (self.tx.clone(), self.inbox_seq);
        self.queue(
            prio,
            || false,
            move || {
                let _ = tx.send(Msg::Inbox(seq, gh::notifications()));
            },
            || {},
        );
    }

    /// `gh api rate_limit` (free): refreshes the shared quota state. Runs even while paused, because
    /// it is how the pause ends.
    fn spawn_rate_poll(&mut self) {
        if !self.net {
            return;
        }
        self.rate_at = Some(std::time::Instant::now());
        self.queue(
            Prio::Probe,
            || false,
            || {
                if let Ok(body) = gh::gh(["api", "rate_limit"]) {
                    rate::apply_poll(&body);
                }
            },
            || {},
        );
    }

    /// No background fetching while the quota is low or GitHub asked us to back off.
    fn bg_quiet(&self) -> bool {
        self.quota_low || self.paused
    }

    /// Unread notifications outside hidden repos; None while unknown.
    pub fn unread_count(&self) -> Option<usize> {
        let hidden = &self.cfg.repos;
        self.unread
            .as_ref()
            .map(|v| v.iter().filter(|r| !hidden.is_hidden(r)).count())
    }

    fn clear_cache(&mut self) {
        self.cache.clear();
        self.dmeta.clear();
        self.revalidating.clear();
        if let Some(h) = self.hl.get_mut() {
            h.clear();
        }
        self.dgen += 1;
        self.dgen_a.store(self.dgen, Relaxed);
    }

    /// `R`: everything again, skipping the on-disk cache.
    fn reload_all(&mut self) {
        self.reload(true);
    }

    /// Reload every loaded panel (`fresh` skips the on-disk cache). The PR and Issues lists come from
    /// one batched request; in the global view they are searches and load on their own.
    fn reload(&mut self, fresh: bool) {
        if fresh {
            self.disk_floor = rate::now();
        }
        self.clear_cache();
        self.counts.clear();
        self.cpending.clear();
        self.count_failed.clear();
        self.cgen += 1;
        self.cgen_a.store(self.cgen, Relaxed);
        if !self.global {
            self.start_load(fresh);
        }
        for i in 0..self.panels.len() {
            let p = &self.panels[i];
            let batched = !self.global && matches!(p.kind, PK::Prs | PK::Issues);
            if !p.unloaded && !batched {
                self.load_panel(i, fresh);
            }
        }
        self.spawn_unread(Prio::User);
    }

    fn load_panel(&mut self, i: usize, fresh: bool) {
        match self.panels[i].kind {
            k if k.is_global_search() => return self.load_global(i, fresh),
            PK::Repos => return self.load_repos_panel(i),
            _ => {}
        }
        let (kind0, tab0) = (self.panels[i].kind, self.panels[i].tab_id());
        let key = self.list_key(kind0, tab0);
        if self.net
            && !fresh
            && self.panels[i].source().is_some()
            && self.show_cached_list(i, key.as_deref())
        {
            return;
        }
        let p = &mut self.panels[i];
        let Some((src, tab_id)) = p.source() else {
            return;
        };
        p.loading = true;
        p.unloaded = false;
        p.error = None;
        p.seq = next_seq();
        p.stamp.store(p.seq, Relaxed);
        if !self.net {
            return;
        }
        let (seq, kind, tab, stamp) = (p.seq, p.kind, p.tab, p.stamp.clone());
        let (tx, tx2, repo) = (self.tx.clone(), self.tx.clone(), self.repo.clone());
        // a list the user already moved past (another tab, a newer load) never starts
        self.queue(
            Prio::User,
            move || stamp.load(Relaxed) != seq,
            move || {
                let res = gh::list(&repo, src, tab_id, fresh);
                if let (Ok(items), Some(k)) = (&res, &key) {
                    dcache::write_list(k, items, 0, 0, rate::now());
                }
                let _ = tx.send(Msg::List(kind, tab, seq, res));
            },
            // lost (queue full, panic): the panel must not sit on "loading..."
            move || {
                let _ = tx2.send(Msg::List(
                    kind,
                    tab,
                    seq,
                    Err("could not load; try again (r)".into()),
                ));
            },
        );
    }

    /// Seconds a cached copy of this list counts as fresh (`[cache] hot_s`, `warm_s`, `cold_s`).
    fn list_ttl(&self, kind: PK, tab_id: usize) -> u64 {
        let c = &self.cfg.cache;
        match (kind, tab_id) {
            (PK::Review, _) | (PK::MyPrs, 0) => c.hot_s,
            (PK::MyPrs, _) => c.cold_s,
            (PK::Prs, t) if t >= 3 => c.cold_s,
            (PK::Prs, _) => c.hot_s,
            _ => c.warm_s,
        }
    }

    /// The disk key of a PR or issue list: everything its rows depend on is in it (scope, window,
    /// favorites, hidden repos, the section's own filter, the repo), so a different view never reads
    /// another's rows. None for lists that are not kept.
    fn list_key(&self, kind: PK, tab_id: usize) -> Option<String> {
        let favs = if self.scope == Scope::Favorites {
            dcache::digest(&self.cfg.repos.favorites)
        } else {
            "-".into()
        };
        let mut hidden = self.cfg.repos.hidden.clone();
        hidden.sort();
        let common = format!(
            "{}-{}-{favs}-{}",
            self.scope.key(),
            self.window.label(),
            dcache::digest(&hidden)
        );
        let name = match kind {
            PK::Review => "review".to_string(),
            PK::MyPrs => "mine".to_string(),
            PK::Assigned => "assigned".to_string(),
            PK::Involved => "involved".to_string(),
            k if k.custom().is_some() => {
                let c = self.cfg.sections.get(k.custom()?)?;
                let repo = if self.global { "" } else { self.repo.as_str() };
                format!(
                    "c{}",
                    dcache::digest([
                        c.title.as_str(),
                        c.filter.as_str(),
                        repo,
                        &format!("{:?}", c.kind)
                    ])
                )
            }
            PK::Prs | PK::Issues if !self.global => {
                let what = if kind == PK::Prs { "prs" } else { "issues" };
                return Some(format!("r-{}-{what}-{tab_id}", self.repo.replace('/', "_")));
            }
            _ => return None,
        };
        Some(format!("g-{name}-{tab_id}-{common}"))
    }

    /// (rows, more than the page holds) as the list's count shows it.
    fn list_count(&self, p: &Panel, n: usize) -> (usize, bool) {
        let cap = p
            .kind
            .custom()
            .and_then(|i| self.cfg.sections.get(i))
            .map_or_else(
                || p.source().map_or(gh::LIMIT, |s| gh::cap(s.0)),
                config::SectionCfg::limit,
            );
        (n, n >= cap)
    }

    /// Show the copy of a list kept on disk, if any. Returns true when it is still fresh and
    /// nothing needs fetching; a stale copy stays on screen, marked, while the refresh runs.
    fn show_cached_list(&mut self, i: usize, key: Option<&str>) -> bool {
        let (kind, tab_id) = (self.panels[i].kind, self.panels[i].tab_id());
        let Some(c) = key.and_then(|k| dcache::read_list(k, rate::now())) else {
            return false;
        };
        let fresh = c.age <= self.list_ttl(kind, tab_id);
        let count = self.list_count(&self.panels[i], c.items.len());
        let p = &mut self.panels[i];
        p.cursor = p.cursor.min(c.items.len().saturating_sub(1));
        p.items = c.items;
        (p.hidden, p.not_shown, p.error, p.unloaded) = (c.hidden, c.not_shown, None, false);
        p.cached_at = (!fresh).then(|| rate::now().saturating_sub(c.age));
        p.loading = !fresh;
        self.counts.insert((kind, tab_id), count);
        fresh
    }

    /// A built-in section's search, narrowed to the window for its pull requests.
    fn section_query(&self, sec: Section, tab: usize) -> Query {
        match global::since(self.window, rate::now()) {
            Some(ts) => Query::Recent(sec, tab, ts),
            None => Query::Section(sec, tab),
        }
    }

    /// `W`: the next window. Only the sections that search pull requests are looked at again.
    fn cycle_window(&mut self) {
        self.window = self.window.next();
        let pr_section = |p: &Panel| matches!(p.kind, PK::Review | PK::MyPrs | PK::Involved);
        for p in self.panels.iter_mut().filter(|p| pr_section(p)) {
            (p.unloaded, p.cursor, p.hidden, p.not_shown) = (true, 0, 0, 0);
            (p.items, p.cached_at) = (vec![], None);
        }
        if let Some(o) = self.other.as_mut() {
            for p in o.panels.iter_mut().filter(|p| pr_section(p)) {
                (p.unloaded, p.cursor, p.hidden, p.not_shown) = (true, 0, 0, 0);
                (p.items, p.cached_at) = (vec![], None);
            }
        }
        let f = self.focus;
        if pr_section(&self.panels[f]) {
            self.load_panel(f, false);
        }
        self.status = format!("pull requests updated in: {}", self.window.label());
        self.reset_view();
        self.rebuild_header();
    }

    /// `g`: flat, by author, by repo.
    fn cycle_group(&mut self) {
        self.group = self.group.next();
        self.panels[self.focus].cursor = 0;
        self.status = format!("group by: {}", self.group.label());
        self.reset_view();
        self.rebuild_header();
    }

    /// A list `g` and `W` apply to: a global search section with the list focused.
    fn on_global_list(&self) -> bool {
        self.global
            && self.panels[self.focus].kind.is_global_search()
            && !self.detail_focus
            && self.ctx.is_none()
    }

    /// A global section: one search per scope chunk, run at user priority when the section is focused,
    /// reloaded or its scope changed. Hidden repos are dropped inside the job.
    fn load_global(&mut self, i: usize, fresh: bool) {
        let kind = self.panels[i].kind;
        let tab_id = self.panels[i].tab_id();
        let query = match kind {
            PK::Review => Ok(self.section_query(Section::Review, tab_id)),
            PK::MyPrs => Ok(self.section_query(Section::MyPrs, tab_id)),
            PK::Assigned => Ok(Query::Section(Section::Assigned, tab_id)),
            k if k.custom().is_some() => self
                .cfg
                .sections
                .get(k.custom().unwrap_or_default())
                .ok_or_else(|| "section is no longer in the config".to_string())
                .and_then(|c| {
                    let repo = (!self.global).then_some(self.repo.as_str());
                    global::Custom::new(c, repo)
                })
                .map(Query::Custom),
            _ => Ok(self.section_query(Section::Involved, tab_id)),
        };
        let query = match query {
            Ok(q) => q,
            Err(e) => {
                let p = &mut self.panels[i];
                (p.loading, p.unloaded, p.error) = (false, false, Some(e));
                return;
            }
        };
        // what is on disk is shown at once; while it is fresh nothing is searched at all
        let key = self.list_key(kind, tab_id);
        if self.net && !fresh && self.show_cached_list(i, key.as_deref()) {
            return;
        }
        // the search API allows 30 requests a minute: never start a refresh the rest of the minute can't pay for
        let needed = query.searches_needed(&self.scope, &self.cfg.repos.favorites);
        if let Some(b) = self.rate.search.filter(|b| rate::now() < b.reset)
            && (b.remaining as usize) < needed
        {
            let msg = format!("search quota low, resets {}", rate::clock(b.reset));
            let p = &mut self.panels[i];
            (p.loading, p.unloaded) = (false, false);
            if p.items.is_empty() {
                p.error = Some(msg.clone());
            }
            self.status = msg;
            return;
        }
        let p = &mut self.panels[i];
        p.loading = true;
        p.unloaded = false;
        p.error = None;
        p.seq = next_seq();
        p.stamp.store(p.seq, Relaxed);
        let (seq, tab, stamp) = (p.seq, p.tab, p.stamp.clone());
        if !self.net {
            return;
        }
        let (scope, favs, hidden) = (
            self.scope.clone(),
            self.cfg.repos.favorites.clone(),
            self.cfg.repos.clone(),
        );
        let quota = self
            .rate
            .search
            .filter(|b| rate::now() < b.reset)
            .map(|b| global::Quota {
                remaining: b.remaining,
                limit: b.limit,
            });
        if scope == Scope::Favorites
            && query.uses_scope()
            && !favs.iter().any(|f| crate::state::valid_repo(f))
        {
            self.status = "no favorites yet: press f on a repo in the Repos panel".into();
        }
        let (tx, tx2) = (self.tx.clone(), self.tx.clone());
        self.queue(
            Prio::User,
            move || stamp.load(Relaxed) != seq,
            move || match query.fetch(&scope, &favs, &hidden, quota) {
                Ok(l) => {
                    if let (Some(k), None) = (&key, &l.note) {
                        dcache::write_list(k, &l.items, l.hidden, l.not_shown, rate::now());
                    }
                    let _ = tx.send(Msg::GMeta(kind, tab, seq, l.hidden, l.not_shown, l.note));
                    let _ = tx.send(Msg::List(kind, tab, seq, Ok(l.items)));
                }
                Err(e) => {
                    let _ = tx.send(Msg::List(kind, tab, seq, Err(e)));
                }
            },
            move || {
                let _ = tx2.send(Msg::List(
                    kind,
                    tab,
                    seq,
                    Err("could not load; try again (r)".into()),
                ));
            },
        );
    }

    /// The Repos panel is local: favorites from the config, recent repos from the state file, details
    /// from the cached repo list. No API call.
    fn load_repos_panel(&mut self, i: usize) {
        let known = read_repos().map(|(c, _)| c.rows).unwrap_or_default();
        let names: Vec<String> = if self.panels[i].tab_id() == 0 {
            self.cfg.repos.favorites.clone()
        } else {
            self.recent.repos.clone()
        };
        let items = global::repo_items(
            &names,
            &known,
            &self.cfg.repos,
            rate::now() as i64,
            &gh::host(),
        );
        let p = &mut self.panels[i];
        p.cursor = p.cursor.min(items.len().saturating_sub(1));
        (p.items, p.loading, p.unloaded, p.error) = (items, false, false, None);
    }

    /// Counts for tabs that aren't showing. `lazy` (default): the focused panel's other tabs once
    /// the user has been idle for a second, never search-backed tabs, never in the global view,
    /// never while the quota is low; `eager`: every panel at once; `off`: none.
    fn fetch_counts(&mut self) {
        let mode = self.cfg.api.counts;
        let lazy = mode == config::Counts::Lazy;
        let idle = std::time::Duration::from_millis(if lazy { 1000 } else { 100 });
        if !self.net
            || self.ctx.is_some()
            || self.global
            || mode == config::Counts::Off
            || self.bg_quiet()
            || !self.cpending.is_empty()
            || self.panels.iter().any(|p| p.loading)
            || self.last_input.elapsed() < idle
        {
            return;
        }
        let focus = self.focus;
        let todo = self.panels.iter().enumerate().find_map(|(i, p)| {
            if (lazy && i != focus) || p.tabs.len() < 2 {
                return None;
            }
            p.tabs.iter().find_map(|t| {
                let key = (p.kind, t.id);
                let known = self.counts.contains_key(&key)
                    || self.cpending.contains(&key)
                    || self.count_failed.contains(&key);
                (!known && !Panel::search_backed(p.kind, t.id))
                    .then(|| source_of(p.kind, t.id).map(|src| (key, src)))
                    .flatten()
            })
        });
        let Some((key, (src, tab))) = todo else {
            return;
        };
        self.cpending.insert(key);
        let (tx, tx2, repo, g) = (
            self.tx.clone(),
            self.tx.clone(),
            self.repo.clone(),
            self.cgen,
        );
        let gen_a = self.cgen_a.clone();
        // the count shares the list's copy on disk: a fresh one answers without a request, and a
        // fetched one is kept for when the tab is opened
        let (ckey, ttl) = (self.list_key(key.0, key.1), self.list_ttl(key.0, key.1));
        self.queue(
            Prio::Background,
            move || gen_a.load(Relaxed) != g,
            move || {
                let now = rate::now();
                let cached = ckey
                    .as_ref()
                    .and_then(|k| dcache::read_list(k, now))
                    .filter(|c| c.age <= ttl);
                let res = match cached {
                    Some(c) => Ok((c.items.len(), c.items.len() >= gh::cap(src))),
                    None => gh::list(&repo, src, tab, false).map(|items| {
                        if let Some(k) = &ckey {
                            dcache::write_list(k, &items, 0, 0, now);
                        }
                        (items.len(), items.len() >= gh::cap(src))
                    }),
                };
                let _ = tx.send(Msg::Count(g, key, res));
            },
            move || {
                let _ = tx2.send(Msg::Count(g, key, Err("dropped".into())));
            },
        );
    }

    /// The panel of this kind, in the active set or the one parked while drilled in.
    fn panel_mut(&mut self, k: PK) -> Option<&mut Panel> {
        let parked = self.ctx.as_mut().map(|c| &mut c.saved);
        let other = self.other.as_mut().map(|s| &mut s.panels);
        self.panels
            .iter_mut()
            .chain(parked.into_iter().flatten())
            .chain(other.into_iter().flatten())
            .find(|p| p.kind == k)
    }

    /// Where the panel that asked for list `seq` lives now: on screen, behind a drill-in, or parked
    /// with the other home. A reply that matches none of them is for something that is gone.
    fn locate(&self, kind: PK, tab: usize, seq: u64) -> Option<(Loc, usize)> {
        let find = |ps: &[Panel]| {
            ps.iter()
                .position(|p| p.kind == kind && p.seq == seq && p.tab == tab)
        };
        if let Some(i) = find(&self.panels) {
            return Some((Loc::Active, i));
        }
        if let Some(i) = self.ctx.as_ref().and_then(|c| find(&c.saved)) {
            return Some((Loc::Ctx, i));
        }
        self.other
            .as_ref()
            .and_then(|o| find(&o.panels))
            .map(|i| (Loc::Other, i))
    }

    #[cfg(test)]
    pub fn panel_idx_for_test(&self, k: PK) -> usize {
        self.panel_idx(k).unwrap()
    }

    fn panel_idx(&self, k: PK) -> Option<usize> {
        self.panels.iter().position(|p| p.kind == k)
    }

    /// Copy the shared quota state and work out what it means for the screen.
    fn sync_rate(&mut self) {
        let (now, st) = (rate::now(), rate::snapshot());
        let (low, pause) = (self.cfg.api.low_quota_percent, self.cfg.api.pause_percent);
        self.quota_low =
            st.low(now, low) || st.search.is_some_and(|b| b.remaining < 5 && now < b.reset);
        self.paused = st.paused(now, pause);
        // when the quota comes back, as a clock time (computed once per distinct reset)
        let reset = st
            .resumes_at(now, pause)
            .or_else(|| st.chip(now, low).map(|c| c.reset));
        if let Some(r) = reset
            && self.rate_clock.as_ref().is_none_or(|(e, _)| *e != r)
        {
            self.rate_clock = Some((r, rate::clock(r)));
        }
        self.rate = st;
    }

    /// The header chip, e.g. `412/5000`, when the quota is low and `rate_header` is on.
    pub fn quota_chip(&self) -> Option<rate::Chip> {
        let c = &self.cfg.api;
        if !c.rate_header {
            return None;
        }
        self.rate.chip(rate::now(), c.low_quota_percent)
    }

    /// What the status line says while background refreshing is paused.
    pub fn pause_note(&self) -> Option<String> {
        if !self.paused {
            return None;
        }
        let at = self
            .rate
            .resumes_at(rate::now(), self.cfg.api.pause_percent)
            .and_then(|r| self.rate_clock.as_ref().filter(|(e, _)| *e == r))
            .map(|(_, c)| format!(", resets {c}"))
            .unwrap_or_default();
        Some(format!("paused background refresh (rate limit low{at})"))
    }

    pub fn poll(&mut self) {
        let mut select_focus = None;
        self.sync_rate();
        if self.net
            && self.status.is_empty()
            && let Some(w) = crate::cache::take_warning()
        {
            self.status = w;
        }
        self.tick = self.tick.wrapping_add(1);
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::List(kind, tab, seq, res) => {
                    let (pending, cur) = (self.pending_select.clone(), self.repo.clone());
                    let Some((loc, idx)) = self.locate(kind, tab, seq) else {
                        continue;
                    };
                    let p = match loc {
                        Loc::Active => &mut self.panels[idx],
                        Loc::Ctx => &mut self.ctx.as_mut().expect("located there").saved[idx],
                        Loc::Other => &mut self.other.as_mut().expect("located there").panels[idx],
                    };
                    p.loading = false;
                    let consumed = pending.as_ref().is_some_and(|x| x.0 == kind);
                    let key = (kind, p.tab_id());
                    let cap = p
                        .kind
                        .custom()
                        .and_then(|i| self.cfg.sections.get(i))
                        .map_or_else(
                            || p.source().map_or(gh::LIMIT, |s| gh::cap(s.0)),
                            config::SectionCfg::limit,
                        );
                    let (mut found, mut rec) = (false, None);
                    let mut kept_err = None;
                    match res {
                        Ok(items) => {
                            p.cursor = p.cursor.min(items.len().saturating_sub(1));
                            if let Some((pk, n, repo)) = pending
                                && pk == p.kind
                                && repo == cur
                                && let Some(pos) = items.iter().position(|i| i.number == n)
                            {
                                p.cursor = pos;
                                select_focus = Some(pk);
                                found = true;
                            }
                            rec = Some((items.len(), items.len() >= cap));
                            p.items = items;
                            p.cached_at = None;
                        }
                        // a copy from disk is on screen: it stays, and the failure goes to the status line
                        Err(e) if p.cached_at.is_some() && !p.items.is_empty() => {
                            kept_err = Some(e)
                        }
                        Err(e) => p.error = Some(e),
                    }
                    if let Some(e) = kept_err {
                        self.status = format!("refresh failed, showing the cached list: {e}");
                    }
                    if consumed {
                        // closed or merged: the lists only hold open items, so show it on GitHub
                        if !found && let Some((pk, n, repo)) = self.pending_select.take() {
                            let what = if pk == PK::Prs { "pull" } else { "issues" };
                            self.status = format!("#{n} is not open: opened on GitHub");
                            if self.net {
                                self.open_url(format!("https://github.com/{repo}/{what}/{n}"));
                            }
                        }
                        self.pending_select = None;
                    }
                    if let Some(r) = rec {
                        // a parked home keeps its own counts
                        if loc == Loc::Other {
                            if let Some(o) = self.other.as_mut() {
                                o.counts.insert(key, r);
                            }
                        } else {
                            self.cpending.remove(&key);
                            self.counts.insert(key, r);
                        }
                    }
                }
                Msg::Count(g, key, res) => {
                    self.cpending.remove(&key);
                    if g == self.cgen {
                        match res {
                            Ok(r) => {
                                self.counts.insert(key, r);
                                self.count_failed.remove(&key);
                            }
                            // a lost job is retried; a real failure waits for the next reload
                            Err(e) if e != "dropped" => {
                                self.count_failed.insert(key);
                            }
                            Err(_) => {}
                        }
                    }
                }
                Msg::Meta(repo, m) => {
                    if !self.global && self.repo == repo {
                        self.meta = Some(m);
                    } else if let Some(o) = self.other.as_mut().filter(|o| o.repo == repo) {
                        o.meta = Some(m);
                    }
                }
                Msg::Inbox(seq, res) => {
                    if seq == self.inbox_seq {
                        self.unread = res
                            .as_ref()
                            .ok()
                            .map(|v| v.iter().map(|i| i.repo.clone()).collect());
                        if let Some(ib) = &mut self.inbox {
                            ib.loading = false;
                            match res {
                                Ok(v) => {
                                    let hidden = &self.cfg.repos;
                                    ib.items = v
                                        .into_iter()
                                        .filter(|i| !hidden.is_hidden(&i.repo))
                                        .collect();
                                    ib.error = None;
                                    ib.cursor = ib.cursor.min(ib.items.len().saturating_sub(1));
                                }
                                Err(e) => ib.error = Some(e),
                            }
                        }
                    }
                }
                Msg::Detail(key, tab, g, res) => {
                    // GitHub's own viewed marks join the local ones (opt-in; never removes any)
                    if g == self.dgen
                        && let Ok(Data::Files(fd)) = &res
                        && !fd.gh_viewed.is_empty()
                        && let Err(e) = self.viewed.merge(&fd.viewed_key, &fd.sha, &fd.gh_viewed)
                    {
                        self.status = e;
                    }
                    if g == self.dgen {
                        let k = (key, tab);
                        let refreshing = self.revalidating.remove(&k);
                        match res {
                            // a failed refresh keeps the copy that is on screen, and waits a minute
                            Err(e)
                                if refreshing
                                    && matches!(self.cache.get(&k), Some(Load::Done(Ok(_)))) =>
                            {
                                if let Some(m) = self.dmeta.get_mut(&k) {
                                    m.hold = rate::now() + 60;
                                }
                                self.status =
                                    format!("refresh failed, showing the cached copy: {e}");
                            }
                            res => {
                                self.cache.insert(k, Load::Done(res));
                            }
                        }
                    }
                }
                Msg::DStamp(key, tab, g, meta) => {
                    if g == self.dgen {
                        self.dmeta.insert((key, tab), meta);
                    }
                }
                Msg::DStale(key, tab, g, data, meta) => {
                    if g == self.dgen {
                        let k = (key, tab);
                        self.cache.insert(k.clone(), Load::Done(Ok(data)));
                        self.revalidating.insert(k.clone());
                        self.dmeta.insert(k, meta);
                    }
                }
                Msg::More(key, g, m) => {
                    if g == self.dgen {
                        let k = (key, Tab::Comments);
                        if let Some(Load::Done(Ok(Data::Comments(cd)))) = self.cache.get_mut(&k) {
                            cd.apply(m);
                        }
                        // the last page has arrived: the whole thread is worth keeping on disk
                        if self.cfg.cache.details
                            && let Some(meta) = self.dmeta.get(&k)
                            && let Some(Load::Done(Ok(d))) = self.cache.get(&k)
                        {
                            dcache::write_detail(
                                &meta.repo,
                                meta.kind,
                                meta.number,
                                Tab::Comments,
                                &meta.stamp,
                                d,
                                rate::now(),
                            );
                        }
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
                Msg::Done(res, then) => {
                    let ok = res.is_ok();
                    self.status = res.unwrap_or_else(|e| e);
                    if let (true, Then::Select(pk)) = (ok, then)
                        && let Some(n) = self
                            .status
                            .trim_end_matches('/')
                            .rsplit('/')
                            .next()
                            .and_then(|s| s.parse().ok())
                    {
                        self.pending_select = Some((pk, n, self.repo.clone()));
                        // the new item must be in the list: show everything open
                        if let Some(p) = self.panel_mut(pk) {
                            p.set_tab_id(2);
                        }
                    }
                    self.reload_all();
                }
                Msg::Branch(repo, b) => {
                    if !self.global && self.repo == repo {
                        self.branch = b.clone().unwrap_or_default();
                        self.cwd_branch = b;
                        self.rebuild_header();
                    } else if let Some(o) = self.other.as_mut().filter(|o| o.repo == repo) {
                        o.branch = b.clone().unwrap_or_default();
                        o.cwd_branch = b;
                    }
                }
                Msg::FormData(seq, d) => {
                    if seq == self.form_seq
                        && let Some(Modal::Form(f)) = &mut self.modal
                    {
                        f.note = None;
                        f.set_labels(d.labels.clone());
                        f.set_templates(d.templates.clone());
                        if matches!(f.spec, form::Spec::Dispatch { .. }) {
                            let base = if d.default_branch.is_empty() {
                                "main"
                            } else {
                                &d.default_branch
                            };
                            f.set_dispatch(
                                base,
                                d.branches.clone(),
                                d.tags.clone(),
                                d.dispatch.clone(),
                            );
                        }
                        if matches!(f.spec, form::Spec::Pr { .. }) {
                            let base = if d.default_branch.is_empty() {
                                "main"
                            } else {
                                &d.default_branch
                            };
                            f.set_pr_data(
                                base,
                                d.branches.clone(),
                                d.head_pushed,
                                d.pr_template.clone(),
                            );
                        }
                    }
                }
                Msg::Repos(g, _) if g != self.repos_seq => {}
                Msg::Repos(_, Ok(m)) => {
                    if let Some(b) = &mut self.browser {
                        if m.replace {
                            b.rows.clear();
                        }
                        if b.viewer.is_empty() || m.replace {
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
                Msg::GMeta(kind, tab, seq, hidden, not_shown, note) => {
                    if let Some((loc, idx)) = self.locate(kind, tab, seq) {
                        let p = match loc {
                            Loc::Active => &mut self.panels[idx],
                            Loc::Ctx => &mut self.ctx.as_mut().expect("located there").saved[idx],
                            Loc::Other => {
                                &mut self.other.as_mut().expect("located there").panels[idx]
                            }
                        };
                        (p.hidden, p.not_shown) = (hidden, not_shown);
                        if loc != Loc::Other {
                            if let Some(n) = note {
                                self.status = n;
                            } else if not_shown > 0 {
                                self.status = format!(
                                    "(+{not_shown} favorites not shown \u{2014} narrow the scope with s)"
                                );
                            }
                        }
                    }
                }
                Msg::Orgs(res) => match res {
                    Ok(v) => {
                        self.orgs = Some(v.into_iter().filter(|o| global::valid_owner(o)).collect())
                    }
                    Err(e) => {
                        self.orgs = Some(vec![]);
                        self.status = format!("could not list your organizations: {e}");
                    }
                },
                Msg::User(g, u) => {
                    if g == self.hgen && !u.is_empty() {
                        self.user = u;
                        self.rebuild_header();
                    }
                }
                Msg::Dropped(key, tab, g) => {
                    if g == self.dgen {
                        self.revalidating.remove(&(key.clone(), tab));
                        if matches!(self.cache.get(&(key.clone(), tab)), Some(Load::Loading)) {
                            self.cache.remove(&(key, tab));
                        }
                    }
                }
                Msg::StartupFallback(repo) => {
                    if !self.global && self.repo == repo {
                        self.fallback_start();
                    }
                }
                Msg::Reactors(item, id, g, res) => {
                    self.react_pending.remove(&id);
                    if g == self.dgen {
                        match res {
                            Ok(names) => {
                                if let Some(Load::Done(Ok(Data::Comments(cd)))) =
                                    self.cache.get_mut(&(item, Tab::Comments))
                                {
                                    for e in &mut cd.entries {
                                        if e.card.set_reactors(&id, &names) {
                                            break;
                                        }
                                    }
                                }
                            }
                            Err(e) => self.status = format!("could not load who reacted: {e}"),
                        }
                    }
                }
            }
        }
        if let Some(pk) = select_focus {
            self.pending_select = None;
            if let Some(i) = self.panel_idx(pk) {
                self.set_focus(i);
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
                ..Default::default()
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
        let Some(it) = self.selected().cloned().filter(Item::repo_ok) else {
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
        // `m` is the user asking; scrolling toward the end is background work that waits for quota
        let prio = if manual { Prio::User } else { Prio::Background };
        let (gen_a, tx2, pend2, key2) =
            (self.dgen_a.clone(), tx.clone(), pend.clone(), key.0.clone());
        self.queue(
            prio,
            move || gen_a.load(Relaxed) != g,
            move || {
                let pr = it.kind == Kind::Pr;
                gh::more_pages(&repo, &it.number.to_string(), pr, pend, 1, &mut |m| {
                    tx.send(Msg::More(key.0.clone(), g, m)).is_ok()
                });
            },
            move || {
                let _ = tx2.send(Msg::More(key2, g, gh::More::Paused(pend2)));
            },
        );
    }

    /// How long the screen must sit still before this tab is fetched: the cheap Overview soon, the
    /// expensive tabs (Comments, Diff, Commits, Checks, logs) after `settle_ms`.
    fn settle(&self, tab: Tab) -> std::time::Duration {
        std::time::Duration::from_millis(if tab == Tab::Overview {
            100
        } else {
            self.cfg.api.settle_ms
        })
    }

    fn fetch(&mut self, it: Item, tab: Tab) {
        if !gh::needs_fetch(it.kind, tab) {
            return;
        }
        let key = (it.key(), tab);
        // a repo name that isn't plain owner/name is shown (neutralized) but never sent to gh
        if !it.repo_ok() {
            self.cache.entry(key).or_insert_with(|| {
                Load::Done(Err(
                    "this repo's name looks unsafe: not contacting GitHub for it".into(),
                ))
            });
            return;
        }
        // added to what queued jobs still want right away; the set is replaced (never cleared first)
        // when the tick ends, so a job can't be judged stale in between
        self.want_now.insert(key.clone());
        if let Ok(mut w) = self.wanted.lock() {
            w.insert(key.clone());
        }
        // trailing debounce for the Overview: only an item the cursor rested on for a whole tick
        // (it was wanted on the previous idle tick too) is fetched, not every row scrolled past
        if tab == Tab::Overview && !self.want_prev.contains(&key) {
            return;
        }
        let now = rate::now();
        let idle = self.net && self.last_input.elapsed() >= self.settle(tab);
        // a copy that is past its time stays on screen while a fresh one is fetched
        let mut refresh = false;
        if let Some(Load::Done(Ok(d))) = self.cache.get(&key) {
            let Some(m) = self.dmeta.get(&key) else {
                return;
            };
            if !idle
                || self.revalidating.contains(&key)
                || now < m.hold
                || dcache::detail_fresh(
                    &self.cfg.cache,
                    tab,
                    d,
                    &m.stamp,
                    &it.updated,
                    now.saturating_sub(m.at),
                )
            {
                return;
            }
            refresh = true;
        } else if self.cache.contains_key(&key) || !idle {
            return;
        }
        if refresh {
            self.revalidating.insert(key.clone());
        } else {
            self.cache.insert(key.clone(), Load::Loading);
        }
        // an item carries its repo (every row of the global home does): that, not the app's repo
        let repo = if it.repo.is_empty() {
            self.repo.clone()
        } else {
            it.repo.clone()
        };
        let (tx, tx2, g) = (self.tx.clone(), self.tx.clone(), self.dgen);
        let (wanted, gen_a, k2) = (self.wanted.clone(), self.dgen_a.clone(), key.clone());
        let key3 = key.clone();
        let disk = self.cfg.cache.details && dcache::cacheable(it.kind);
        let (ccfg, floor) = (
            self.cfg.cache.clone(),
            self.disk_floor
                .max(self.item_floor.get(&key.0).copied().unwrap_or(0)),
        );
        self.queue(
            Prio::User,
            // the cache was cleared, or the screen no longer asks for this item
            move || gen_a.load(Relaxed) != g || !wanted.lock().is_ok_and(|w| w.contains(&k2)),
            move || {
                let now = rate::now();
                let meta = |stamp: String, at: u64| DMeta {
                    stamp,
                    at,
                    hold: 0,
                    repo: repo.clone(),
                    kind: it.kind,
                    number: it.number,
                };
                // a copy kept from an earlier run: used as it is when still good, else shown while refreshing
                if disk
                    && !refresh
                    && let Some(dd) =
                        dcache::read_detail(&repo, it.kind, it.number, tab, now, floor)
                {
                    let m = meta(dd.stamp.clone(), now.saturating_sub(dd.age));
                    if dcache::detail_fresh(&ccfg, tab, &dd.data, &dd.stamp, &it.updated, dd.age) {
                        let _ = tx.send(Msg::DStamp(key.0.clone(), tab, g, m));
                        let _ = tx.send(Msg::Detail(key.0.clone(), tab, g, Ok(dd.data)));
                        return;
                    }
                    let _ = tx.send(Msg::DStale(key.0.clone(), tab, g, dd.data, m));
                }
                let res = gh::detail(&repo, &it, tab);
                if let Ok(d) = &res {
                    if disk {
                        dcache::write_detail(&repo, it.kind, it.number, tab, &it.updated, d, now);
                    }
                    if dcache::cacheable(it.kind) {
                        let m = meta(it.updated.clone(), now);
                        let _ = tx.send(Msg::DStamp(key.0.clone(), tab, g, m));
                    }
                }
                // Comments arrive in pages: the first at once, the rest as background work
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
                    let (pr, n) = (it.kind == Kind::Pr, it.number.to_string());
                    let (tx_run, tx_drop) = (tx.clone(), tx.clone());
                    let (k_run, k_drop, pend_drop) = (key.0.clone(), key.0.clone(), pend.clone());
                    pool::global().submit(
                        Prio::Background,
                        || false,
                        move || {
                            // quota low or backing off: leave the rest to scrolling or `m`
                            if rate::quiet_now() {
                                let _ = tx_run.send(Msg::More(k_run, g, gh::More::Paused(pend)));
                                return;
                            }
                            gh::more_pages(&repo, &n, pr, pend, gh::AUTO_PAGES, &mut |m| {
                                tx_run.send(Msg::More(k_run.clone(), g, m)).is_ok()
                            });
                        },
                        move || {
                            let _ = tx_drop.send(Msg::More(k_drop, g, gh::More::Paused(pend_drop)));
                        },
                    );
                }
            },
            move || {
                let _ = tx2.send(Msg::Dropped(key3.0, key3.1, g));
            },
        );
    }

    /// Fetches whatever the screen needs and isn't cached yet; called when input is idle. Detail
    /// fetches still queued for anything this tick no longer asks for are dropped before they start.
    pub fn ensure(&mut self) {
        self.want_now.clear();
        self.ensure_tick();
        self.want_prev = self.want_now.clone();
        if let Ok(mut w) = self.wanted.lock() {
            *w = std::mem::take(&mut self.want_now);
        }
    }

    fn ensure_tick(&mut self) {
        self.fetch_counts();
        // the only polling there is: the badge every 2 minutes (background, pauses with the quota) and
        // the free quota check every 5
        if !self.bg_quiet()
            && self
                .unread_at
                .is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(120))
        {
            self.spawn_unread(Prio::Background);
        }
        if self
            .rate_at
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(300))
        {
            self.spawn_rate_poll();
        }
        // scrolling toward the end of loaded comments pulls the next page, one at a time
        if self.comments_data().is_some_and(gh::CommentsData::paused) && self.near_comments_end() {
            self.load_more(false);
        }
        let kind = self.panels[self.focus].kind;
        // The PR behind Files/Checks/Comments: only fetched while the user is working on PRs.
        if let Some(pr) = self.pr_item().filter(|p| p.kind == Kind::Pr).cloned()
            && (self.ctx.is_some() || kind.is_pr_list() || kind == PK::Files)
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
        let mut v: Vec<&Item> = p.items.iter().filter(hit).collect();
        if self.global && self.group != GroupBy::None && p.kind.is_global_search() {
            self.group_sort(&mut v);
        }
        v
    }

    /// Items of one group together, the group with the longest-waiting item first (ISO times sort as
    /// text); inside a group the search's newest-first order is kept.
    fn group_sort(&self, v: &mut [&Item]) {
        let mut oldest: std::collections::HashMap<String, &str> = Default::default();
        for it in v.iter() {
            let e = oldest
                .entry(self.group.key(it).to_lowercase())
                .or_insert(&it.updated);
            if it.updated.as_str() < *e {
                *e = &it.updated;
            }
        }
        v.sort_by(|a, b| {
            let (ka, kb) = (
                self.group.key(a).to_lowercase(),
                self.group.key(b).to_lowercase(),
            );
            (oldest[&ka], &ka).cmp(&(oldest[&kb], &kb))
        });
    }

    /// (rows, more than that) of the panel's showing tab; None while its first load is out.
    pub fn active_count(&self, i: usize) -> Option<(usize, bool)> {
        let p = &self.panels[i];
        // a failed load has no count (never "0"): only successful loads set one
        if p.error.is_some() && p.items.is_empty() {
            return None;
        }
        if (p.loading || p.unloaded) && p.items.is_empty() {
            return self.counts.get(&(p.kind, p.tab_id())).copied();
        }
        let items = self.visible(i);
        // a trailing "N more" row is not an item; the count says "n+" while more are still coming
        let more = items.last().is_some_and(|x| x.state == "more");
        let n = items.len() - usize::from(more);
        let cap = p
            .kind
            .custom()
            .and_then(|i| self.cfg.sections.get(i))
            .map_or_else(
                || p.source().map_or(gh::LIMIT, |s| gh::cap(s.0)),
                config::SectionCfg::limit,
            );
        // list sources are capped; Files/Checks/... are complete, except Files at GitHub's 3000-file ceiling
        let capped =
            (!p.kind.derived() && p.items.len() >= cap) || (p.kind == PK::Files && n >= 3000);
        Some((n, capped || more))
    }

    /// The tab's list (or its count lookup) failed: the title shows a cross instead of a number.
    pub fn tab_failed(&self, i: usize, ti: usize) -> bool {
        let p = &self.panels[i];
        if ti == p.tab {
            p.error.is_some() && p.items.is_empty()
        } else {
            p.tabs
                .get(ti)
                .is_some_and(|t| self.count_failed.contains(&(p.kind, t.id)))
        }
    }

    /// Count of tab `ti` of panel `i`: live for the showing tab, as fetched for the others.
    pub fn tab_count(&self, i: usize, ti: usize) -> Option<(usize, bool)> {
        let p = &self.panels[i];
        // the Repos panel is local: both of its counts are known without asking anyone
        if p.kind == PK::Repos {
            let n = match p.tabs.get(ti)?.id {
                0 => self
                    .cfg
                    .repos
                    .favorites
                    .iter()
                    .filter(|f| crate::state::valid_repo(f))
                    .count(),
                _ => self.recent.repos.len(),
            };
            return Some((n, false));
        }
        if ti == p.tab {
            self.active_count(i)
        } else {
            self.counts.get(&(p.kind, p.tabs.get(ti)?.id)).copied()
        }
    }

    /// Will the count of tab `ti` of panel `i` be fetched by itself? (`?` is shown when not.)
    pub fn count_wanted(&self, i: usize, ti: usize) -> bool {
        let p = &self.panels[i];
        let Some(t) = p.tabs.get(ti) else {
            return false;
        };
        self.cfg.api.counts != config::Counts::Off
            && !self.global
            && !Panel::search_backed(p.kind, t.id)
    }

    /// Open PRs of the repo, when the PR list's "All" tab has been counted.
    pub fn open_prs(&self) -> Option<(usize, bool)> {
        if self.global {
            return None;
        }
        self.counts.get(&(PK::Prs, 2)).copied()
    }

    /// `hide_empty`: drawn as a single line (never the focused panel, never in a PR drill-in).
    pub fn collapsed(&self, i: usize) -> bool {
        self.cfg.panels.hide_empty && self.ctx.is_none() && i != self.focus && self.panel_empty(i)
    }

    /// Nothing in any of the panel's tabs (and nothing still loading): `hide_empty` collapses it.
    pub fn panel_empty(&self, i: usize) -> bool {
        let p = &self.panels[i];
        if p.loading || p.error.is_some() {
            return false;
        }
        (0..p.tabs.len().max(1)).all(|ti| {
            if p.tabs.is_empty() || ti == p.tab {
                p.items.is_empty()
            } else {
                self.tab_count(i, ti) == Some((0, false))
            }
        })
    }

    pub fn selected_in(&self, i: usize) -> Option<&Item> {
        let v = self.visible(i);
        v.get(self.panels[i].cursor.min(v.len().saturating_sub(1)))
            .copied()
    }

    /// The PR list the Files panel follows: the one last focused, else the first there is.
    fn pr_panel(&self) -> Option<usize> {
        self.panels
            .get(self.pr_src)
            .filter(|p| p.kind.is_pr_list())
            .map(|_| self.pr_src)
            .or_else(|| self.panels.iter().position(|p| p.kind.is_pr_list()))
    }

    /// The pull request the Files/Checks/Comments panels describe.
    pub fn pr_item(&self) -> Option<&Item> {
        match &self.ctx {
            Some(c) => Some(&c.pr),
            None => self
                .pr_panel()
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
            .map_or(self.file0, |i| self.panels[i].cursor)
    }

    fn set_file(&mut self, v: usize) {
        match self.panel_idx(PK::Files) {
            Some(i) => self.panels[i].cursor = v,
            None => self.file0 = v,
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
            k if k.is_pr_list() => {
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
        self.last_input = std::time::Instant::now();
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
                if let Some(t) = hit
                    .ptabs
                    .iter()
                    .find(|t| t.y == pos.y && pos.x >= t.x0 && pos.x < t.x1)
                {
                    self.set_focus(t.panel);
                    self.select_tab(t.panel, t.tab);
                    return;
                }
                if let Some(i) = hit.panels.iter().position(|r| r.contains(pos)) {
                    let was_list = i == self.focus && !self.detail_focus;
                    if !was_list {
                        self.set_focus(i);
                    }
                    if was_list && hit.list.contains(pos) {
                        let mut r = hit.list_off + (pos.y - hit.list.y) as usize;
                        if !hit.list_rows.is_empty() {
                            // a grouped list has heading rows: click maps to the item behind the row
                            r = hit
                                .list_rows
                                .get(r)
                                .copied()
                                .flatten()
                                .unwrap_or(usize::MAX);
                        }
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
        if self.panels[self.focus].kind.is_pr_list() && self.ctx.is_none() {
            self.pr_src = self.focus;
        }
        if self.panels[self.focus].unloaded && self.ctx.is_none() {
            self.load_panel(self.focus, false);
        }
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
            let to = (p.tab as isize + d).rem_euclid(p.tabs.len() as isize) as usize;
            self.select_tab(self.focus, to);
        }
    }

    /// Show the panel's tab number `to` (an index into its tabs).
    fn select_tab(&mut self, i: usize, to: usize) {
        let p = &mut self.panels[i];
        if to >= p.tabs.len() || to == p.tab {
            return;
        }
        (p.tab, p.cursor) = (to, 0);
        p.items.clear();
        self.reset_view();
        self.load_panel(i, false);
    }

    /// Returns true to quit.
    /// Bracketed paste: literal text into whichever text box has focus; anything else ignores it.
    pub fn on_paste(&mut self, s: &str) {
        let clean = |multi: bool| -> String {
            s.replace("\r\n", "\n")
                .chars()
                .filter(|c| (*c == '\n' && multi) || !c.is_control())
                .collect()
        };
        match &mut self.modal {
            Some(Modal::Form(f)) => f.paste(s),
            Some(Modal::Input { buf, .. }) => buf.push_str(&clean(true)),
            Some(_) => {}
            None => {
                if let Some(b) = self.browser.as_mut().filter(|b| b.typing) {
                    b.query.push_str(&clean(false));
                    b.cursor = 0;
                } else if self.typing && self.browser.is_none() && !self.help {
                    self.filter.push_str(&clean(false));
                    self.panels.iter_mut().for_each(|p| p.cursor = 0);
                    self.reset_view();
                }
            }
        }
    }

    pub fn on_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') {
            return true;
        }
        self.last_input = std::time::Instant::now();
        self.status.clear();
        if let Some(m) = self.modal.take() {
            self.modal = self.modal_key(m, k, ctrl);
            return false;
        }
        if self.browser.is_some() {
            self.browser_key(k);
            return false;
        }
        if self.inbox.is_some() {
            self.inbox_key(k);
            return false;
        }
        if self.help {
            self.help_key(k);
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
            KeyCode::Char(c @ '1'..='7') if (c as usize - '1' as usize) < self.panels.len() => {
                let i = c as usize - '1' as usize;
                // the number of the focused panel again steps to its next list tab
                if i == self.focus && !self.detail_focus {
                    self.panel_tab_step(1);
                } else {
                    self.set_focus(i);
                }
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
            KeyCode::Char('H') if self.on_repo_row() || self.on_global_row() => self.toggle_hide(),
            KeyCode::Char('l') | KeyCode::Right if self.selected().is_some() => {
                self.detail_focus = true
            }
            KeyCode::Char('h') | KeyCode::Left => self.detail_focus = false,
            KeyCode::Char('t') if self.diff_active() => {
                (self.mode, self.row) = (self.mode.next(), 0);
                self.scroll.set(0);
            }
            KeyCode::Char('d')
                if !self.detail_focus
                    && self.panels[self.focus].kind == PK::Actions
                    && self
                        .selected()
                        .is_some_and(|i| i.kind == Kind::Workflow && i.state == "active") =>
            {
                let it = self.selected().cloned().unwrap_or_default();
                self.modal = self.form_for(FormKind::Dispatch(
                    it.number.to_string(),
                    it.title.clone(),
                    it.cmd.clone(),
                ));
            }
            KeyCode::Char('n') if self.diff_active() => self.file_step(1),
            KeyCode::Char('n')
                if !self.detail_focus
                    && matches!(self.panels[self.focus].kind, PK::Issues | PK::Prs) =>
            {
                let kind = if self.panels[self.focus].kind == PK::Issues {
                    FormKind::Issue
                } else {
                    FormKind::PrCwd
                };
                self.modal = self.form_for(kind);
            }
            KeyCode::Char('p') if self.diff_active() => self.file_step(-1),
            KeyCode::Char('j') | KeyCode::Down => self.nav(1),
            KeyCode::Char('k') | KeyCode::Up => self.nav(-1),
            KeyCode::Char('d') | KeyCode::Char('u') if ctrl && self.comment_rows() => {
                self.page_comments(if k.code == KeyCode::Char('d') { 1 } else { -1 })
            }
            KeyCode::Char('e') if self.cur_tab() == Tab::Comments => self.toggle_card(true),
            KeyCode::Char('d') if ctrl => self.nav((self.view_h.get() / 2) as isize),
            KeyCode::Char('u') if ctrl => self.nav(-((self.view_h.get() / 2) as isize)),
            KeyCode::Char('g') if self.on_global_list() => self.cycle_group(),
            KeyCode::Char('W') if self.on_global_list() => self.cycle_window(),
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
        if self.selected().is_some_and(|i| !i.repo_ok()) {
            self.status = "this repo's name looks unsafe: actions are disabled for it".into();
            return;
        }
        let mut items = act::actions(
            self.selected(),
            self.panels[self.focus].source().map_or(99, |s| s.0),
            &self.repo,
            &self.sel(),
        );
        if let Some(f) = filter {
            items.retain(|a| a.label.starts_with(f));
        }
        match items.len() {
            0 => self.status = "no such action here".into(),
            1 if filter.is_some() => self.modal = self.pick(items.remove(0)),
            _ => self.modal = Some(Modal::Menu(items, 0)),
        }
    }

    fn pick(&mut self, a: Action) -> Option<Modal> {
        if let Some(kind) = a.form {
            return self.form_for(kind);
        }
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

    /// Open the create-issue / create-PR form and start fetching labels, templates and branches.
    fn form_for(&mut self, kind: FormKind) -> Option<Modal> {
        let repo = self.repo.clone();
        let head = match kind {
            FormKind::Issue => None,
            FormKind::Dispatch(id, name, path) => {
                self.form_seq += 1;
                let (tx, seq, repo2) = (self.tx.clone(), self.form_seq, repo.clone());
                self.user_job(move || {
                    let d = gh::form_data(&repo2, None, Some(&path));
                    let _ = tx.send(Msg::FormData(seq, Box::new(d)));
                });
                return Some(Modal::Form(Box::new(Form::dispatch(&repo, &id, &name))));
            }
            FormKind::Pr(h) => Some(h),
            FormKind::PrCwd => match self.cwd_branch.clone() {
                Some(h) => Some(h),
                None => {
                    self.status = format!(
                        "the current directory is not a clone of {repo} (or no branch is checked out); use the Branches panel"
                    );
                    return None;
                }
            },
        };
        let form = match &head {
            Some(h) => Form::pr(&repo, h),
            None => Form::issue(&repo),
        };
        self.form_seq += 1;
        let (tx, seq) = (self.tx.clone(), self.form_seq);
        self.user_job(move || {
            let d = gh::form_data(&repo, head.as_deref(), None);
            let _ = tx.send(Msg::FormData(seq, Box::new(d)));
        });
        Some(Modal::Form(Box::new(form)))
    }

    fn run_act(&mut self, a: Act, k: KeyEvent) -> bool {
        match a {
            Act::Quit => return true,
            Act::Help => self.help = true,
            Act::Filter if !self.detail_focus => self.typing = true,
            Act::Filter => {}
            Act::Refresh => self.refresh_selected(),
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
            Act::Zoom if self.on_repo_row() => self.toggle_favorite(),
            Act::Zoom => self.zoom = !self.zoom,
            Act::Browser => self.open_browser(false),
            Act::Inbox => self.open_inbox(),
            Act::Global => {
                if !self.detail_focus
                    && self.log.is_none()
                    && self.ctx.is_none()
                    && self.dk().is_none()
                {
                    self.toggle_home();
                } else if k.code == KeyCode::Char('G') {
                    // Derived panels and drill-in: G is "last row" like End; say why it isn't the toggle.
                    self.nav(isize::MAX);
                    if !self.detail_focus && self.log.is_none() {
                        self.status =
                            "G jumps to the last row here; the global home toggles from the lists"
                                .into();
                    }
                } else {
                    self.status = "the global home toggles from the lists".into();
                }
            }
            Act::Scope => {
                if self.on_repo_row() {
                    let r = self
                        .selected_in(self.focus)
                        .map(|i| i.repo.clone())
                        .unwrap_or_default();
                    self.set_scope(Scope::Repo(r));
                    if !self.global {
                        self.toggle_home();
                    }
                } else if self.global {
                    self.modal = Some(Modal::Scope(ScopePicker {
                        stage: ScopeStage::Top,
                        cursor: 0,
                        query: String::new(),
                        names: vec![],
                    }));
                } else {
                    self.status =
                        "scope narrows the global home: press G, or s on a repo in the Repos panel"
                            .into();
                }
            }
            Act::SwitchRepoContext => {
                if self.on_repo_row() {
                    self.open_repo_row();
                } else {
                    self.open_repo_context();
                }
            }
        }
        false
    }

    /// Open the full-screen repo browser and page through every repo the user can access.
    fn open_browser(&mut self, fresh: bool) {
        self.repos_seq += 1;
        self.repos_a.store(self.repos_seq, Relaxed);
        let mut b = Browser::new();
        let mut cached = false;
        // stale-while-revalidate: cached rows appear at once; older than the TTL they are then
        // replaced by a fresh listing (`r` in the browser skips the cache)
        let mut stale = true;
        if !fresh && let Some((c, age)) = read_repos() {
            (b.viewer, b.rows, b.truncated) = (c.viewer, c.rows, c.truncated);
            stale = age > self.cfg.cache.slow_s;
            (b.loading, cached) = (stale, true);
        }
        self.browser = Some(b);
        if stale {
            let j = BrowseJob {
                tx: self.tx.clone(),
                seq: self.repos_seq,
                seq_a: self.repos_a.clone(),
                cached,
                all: vec![],
                total: 0,
            };
            if self.net {
                browse_step(j, None);
            }
        }
    }

    fn open_inbox(&mut self) {
        self.inbox = Some(Inbox {
            items: vec![],
            cursor: 0,
            loading: true,
            error: None,
        });
        self.spawn_unread(Prio::User);
    }

    #[cfg(test)]
    pub fn seed_inbox(&mut self, items: Vec<Item>) {
        self.unread = Some(items.iter().map(|i| i.repo.clone()).collect());
        self.inbox = Some(Inbox {
            items,
            cursor: 0,
            loading: false,
            error: None,
        });
    }

    #[cfg(test)]
    pub fn seed_unread(&mut self, repos: &[&str]) {
        self.unread = Some(repos.iter().map(|r| r.to_string()).collect());
    }

    fn inbox_key(&mut self, k: KeyEvent) {
        let Some(ib) = self.inbox.as_mut() else {
            return;
        };
        let last = ib.items.len().saturating_sub(1);
        let to = |c: usize, d: isize| c.saturating_add_signed(d).min(last);
        if self.keys.get(&k) == Some(Act::Inbox) {
            self.inbox = None;
            return;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.inbox = None,
            KeyCode::Char('j') | KeyCode::Down => ib.cursor = to(ib.cursor, 1),
            KeyCode::Char('k') | KeyCode::Up => ib.cursor = to(ib.cursor, -1),
            KeyCode::Char('g') | KeyCode::Home => ib.cursor = 0,
            KeyCode::Char('G') | KeyCode::End => ib.cursor = last,
            KeyCode::Char('r') => {
                ib.loading = true;
                self.spawn_unread(Prio::User);
            }
            KeyCode::Char('o') => {
                if let Some(url) = ib.items.get(ib.cursor).map(|i| i.url.clone()) {
                    self.open_url(url);
                }
            }
            KeyCode::Char('m') => match ib.items.get(ib.cursor).map(|i| i.cmd.clone()) {
                // thread ids are numeric; anything else never reaches a URL path
                Some(id) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => {
                    self.modal = Some(Modal::Confirm(Confirm::new(act::mark_read(&id), None)));
                }
                _ => self.status = "nothing to mark read".into(),
            },
            KeyCode::Enter => self.inbox_open(),
            _ => {}
        }
    }

    /// Enter in the inbox: show the PR/issue in the lists (switching repo if needed); anything else opens in the browser.
    fn inbox_open(&mut self) {
        let Some(it) = self
            .inbox
            .as_ref()
            .and_then(|ib| ib.items.get(ib.cursor))
            .cloned()
        else {
            return;
        };
        let pk = match it.kind {
            Kind::Pr => PK::Prs,
            Kind::Issue => PK::Issues,
            _ => return self.open_url(it.url),
        };
        if it.number == 0 {
            return self.open_url(it.url);
        }
        // from the global home (or another repo) the item's repo becomes the app's repo first
        let switch = self.global || !it.repo.eq_ignore_ascii_case(&self.repo);
        if !switch && self.panel_idx(pk).is_none() {
            self.status =
                "that panel is hidden in [panels]; press o to open it in the browser".into();
            return;
        }
        self.inbox = None;
        if switch {
            self.go_repo(it.repo.clone());
        }
        let Some(idx) = self.panel_idx(pk) else {
            self.status =
                "that panel is hidden in [panels]; press o to open it in the browser".into();
            return;
        };
        // the lists only hold open items: look in the broadest open tab
        self.pending_select = Some((pk, it.number, self.repo.clone()));
        let p = &mut self.panels[idx];
        p.set_tab_id(2);
        p.cursor = 0;
        self.load_panel(idx, false);
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
            Out::Reload => self.open_browser(true),
            Out::Switch(r) => self.go_repo(r),
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
                let before = self.cfg.repos.clone();
                let on = self.cfg.repos.toggle_hidden(&r);
                if self.save_cfg() {
                    self.status = format!("{} {r}", if on { "hidden:" } else { "unhidden:" });
                    self.hidden_changed(&r, on);
                } else {
                    self.cfg.repos = before;
                }
            }
        }
    }

    /// Same as starting with `-R repo`.
    fn switch_repo(&mut self, repo: String) {
        self.browser = None;
        self.exit_ctx();
        self.repo = repo;
        self.touch_recent();
        (self.cwd_branch, self.pending_select) = (None, None);
        (self.branch, self.meta) = (String::new(), None);
        self.rebuild_header();
        let focus = self.focus;
        for (i, p) in self.panels.iter_mut().enumerate() {
            p.cursor = 0;
            // REST-backed panels are looked at again before they are fetched again
            p.unloaded =
                i != focus && !matches!(p.kind, PK::Prs | PK::Issues | PK::Files | PK::Repos);
            // a section searched the old repo: its rows and count must not outlive the switch
            if p.kind.custom().is_some() {
                (p.items, p.error, p.hidden, p.not_shown) = (vec![], None, 0, 0);
            }
        }
        self.refresh_repos_panels();
        self.reset_view();
        self.spawn_header();
        self.reload(false);
    }

    /// Park the home on screen so the other can take its place.
    fn take_side(&mut self) -> Side {
        Side {
            panels: std::mem::take(&mut self.panels),
            focus: self.focus,
            repo: std::mem::take(&mut self.repo),
            meta: self.meta.take(),
            counts: std::mem::take(&mut self.counts),
            branch: std::mem::take(&mut self.branch),
            cwd_branch: self.cwd_branch.take(),
            filter: std::mem::take(&mut self.filter),
            pr_src: self.pr_src,
            from_global: self.from_global,
        }
    }

    /// Show `side`, building its panels first when it has none yet; true when it was built.
    fn put_side(&mut self, side: Side, to_global: bool) -> bool {
        let built = side.panels.is_empty();
        self.panels = match (built, to_global) {
            (false, _) => side.panels,
            (true, true) => build_global_panels(&self.cfg),
            (true, false) => build_panels(&self.cfg),
        };
        self.focus = if built {
            0
        } else {
            side.focus.min(self.panels.len() - 1)
        };
        self.repo = side.repo;
        self.meta = side.meta;
        self.counts = side.counts;
        self.branch = side.branch;
        self.cwd_branch = side.cwd_branch;
        self.filter = side.filter;
        self.from_global = side.from_global;
        self.pr_src = if built {
            self.panels
                .iter()
                .position(|p| p.kind.is_pr_list())
                .unwrap_or(0)
        } else {
            side.pr_src
        };
        self.global = to_global;
        built
    }

    /// `G`: swap between the repo and the global home, each exactly as it was left (cursor, tab, filter).
    fn toggle_home(&mut self) {
        let to_global = !self.global;
        let next = match self.other.take() {
            Some(s) if to_global || !s.repo.is_empty() => s,
            Some(s) => {
                self.other = Some(s);
                self.status = "no repo yet: press S on an item, or Enter on a repo".into();
                return;
            }
            None if to_global => Side::empty(String::new()),
            None => {
                self.status = "no repo yet: press S on an item, or Enter on a repo".into();
                return;
            }
        };
        self.browser = None;
        let cur = self.take_side();
        let built = self.put_side(next, to_global);
        self.other = Some(cur);
        // counts in flight belong to the home that just left
        self.cgen += 1;
        self.cgen_a.store(self.cgen, Relaxed);
        (self.cpending, self.count_failed) = (HashSet::new(), HashSet::new());
        self.detail_focus = false;
        self.reset_view();
        self.rebuild_header();
        if built && to_global {
            self.load_global_side();
        } else if built {
            self.load_repo_side();
        } else {
            let f = self.focus;
            if self.panels[f].unloaded {
                self.load_panel(f, false);
            }
        }
    }

    /// Open `repo` as the app's repo (from the browser, an inbox item, `S`, a Repos row). From the
    /// global home that puts the repo on screen with `G` leading back; from a repo it just switches.
    fn go_repo(&mut self, repo: String) {
        if !self.global {
            return self.switch_repo(repo);
        }
        let keep = self
            .other
            .as_ref()
            .is_some_and(|s| s.repo.eq_ignore_ascii_case(&repo) && !s.panels.is_empty());
        if !keep {
            self.other = Some(Side::empty(repo));
        }
        self.toggle_home();
        self.from_global = true;
        self.rebuild_header();
    }

    /// `S`: the selected item's repo as the app's repo.
    fn open_repo_context(&mut self) {
        if !self.global {
            self.status = "already in a repo; G goes to the global home".into();
            return;
        }
        match self.selected().map(|i| i.repo.clone()) {
            Some(r) if crate::state::valid_repo(&r) => self.go_repo(r),
            _ => self.status = "nothing selected to open".into(),
        }
    }

    /// Narrow the global home. Persisted (state dir, not config); sections reload when next focused.
    fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
        if let Some(d) = &self.state_dir
            && let Err(e) =
                crate::state::save_scope(&d.join("scope.json"), &gh::host(), &self.scope.key())
        {
            self.status = format!("scope not remembered: {e}");
        }
        self.stale_sections(false);
    }

    /// Searched sections are emptied and reload when focused (the focused one now). A section with a
    /// scope qualifier of its own does not depend on the scope: only `all` (a hidden repo came back) reloads it.
    fn stale_sections(&mut self, all: bool) {
        let secs = self.cfg.sections.clone();
        let stale = |panels: &mut Vec<Panel>| {
            for p in panels.iter_mut().filter(|p| {
                p.kind.is_global_search()
                    && (all || p.kind.custom().is_none_or(|i| scope_applies(secs.get(i))))
            }) {
                (p.unloaded, p.cursor, p.hidden, p.not_shown) = (true, 0, 0, 0);
                (p.items, p.cached_at) = (vec![], None);
            }
        };
        // unhiding (`all`) also concerns the parked home: its sections may have lacked that repo's rows
        if self.global || all {
            stale(&mut self.panels);
            let f = self.focus;
            if self.panels[f].unloaded && self.panels[f].kind.is_global_search() {
                self.load_panel(f, false);
            }
        }
        if (!self.global || all)
            && let Some(o) = self.other.as_mut()
        {
            stale(&mut o.panels);
        }
        self.reset_view();
        self.rebuild_header();
    }

    fn fetch_orgs(&mut self) {
        if self.orgs.is_some() {
            return;
        }
        let tx = self.tx.clone();
        let tx2 = tx.clone();
        self.user_job_or(
            move || {
                let _ = tx.send(Msg::Orgs(global::orgs()));
            },
            move || {
                let _ = tx2.send(Msg::Orgs(Err("request lost".into())));
            },
        );
    }

    /// Rows the picker offers at its current stage: (label, what choosing it does).
    pub fn scope_choices(&self, p: &ScopePicker) -> Vec<(String, ScopeChoice)> {
        let q = p.query.to_lowercase();
        let has = |s: &str| q.is_empty() || s.to_lowercase().contains(&q);
        match p.stage {
            ScopeStage::Top => vec![
                ("All repos".into(), ScopeChoice::Set(Scope::All)),
                (
                    format!("Favorites ({})", self.cfg.repos.favorites.len()),
                    ScopeChoice::Set(Scope::Favorites),
                ),
                ("Org...".into(), ScopeChoice::Stage(ScopeStage::Orgs)),
                ("Repo...".into(), ScopeChoice::Stage(ScopeStage::Repos)),
            ],
            ScopeStage::Orgs => {
                let mut v: Vec<(String, ScopeChoice)> = self
                    .orgs
                    .iter()
                    .flatten()
                    .filter(|o| has(o))
                    .map(|o| (o.clone(), ScopeChoice::Set(Scope::Org(o.clone()))))
                    .collect();
                // any org or user can be searched, not just the ones you belong to
                if global::valid_owner(&p.query)
                    && !v.iter().any(|(n, _)| n.eq_ignore_ascii_case(&p.query))
                {
                    v.push((
                        format!("Use org '{}'", p.query),
                        ScopeChoice::Set(Scope::Org(p.query.clone())),
                    ));
                }
                v
            }
            ScopeStage::Repos => {
                let mut names: Vec<String> = vec![];
                for n in &p.names {
                    if has(n) && !names.iter().any(|x| x.eq_ignore_ascii_case(n)) {
                        names.push(n.clone());
                    }
                }
                names.truncate(50);
                let mut v: Vec<(String, ScopeChoice)> = names
                    .into_iter()
                    .map(|n| (n.clone(), ScopeChoice::Set(Scope::Repo(n))))
                    .collect();
                // a full owner/name typed in is accepted even when no list knows it
                if crate::state::valid_repo(&p.query)
                    && !v.iter().any(|(n, _)| n.eq_ignore_ascii_case(&p.query))
                {
                    v.insert(
                        0,
                        (
                            format!("use {}", p.query),
                            ScopeChoice::Set(Scope::Repo(p.query.clone())),
                        ),
                    );
                }
                v
            }
        }
    }

    /// Candidates for the Repos stage; one disk read of the cached repo list.
    fn scope_repo_names(&self) -> Vec<String> {
        let known = read_repos().map(|(c, _)| c.rows).unwrap_or_default();
        let mut names: Vec<String> = vec![];
        for n in self
            .cfg
            .repos
            .favorites
            .iter()
            .chain(self.recent.repos.iter())
            .chain(known.iter().map(|r| &r.name))
        {
            if crate::state::valid_repo(n) && !names.iter().any(|x| x.eq_ignore_ascii_case(n)) {
                names.push(n.clone());
            }
        }
        names
    }

    fn scope_key(&mut self, mut p: ScopePicker, k: KeyEvent) -> Option<Modal> {
        let typing = p.stage != ScopeStage::Top;
        let n = self.scope_choices(&p).len();
        match k.code {
            KeyCode::Esc => return None,
            KeyCode::Down => p.cursor = (p.cursor + 1).min(n.saturating_sub(1)),
            KeyCode::Up => p.cursor = p.cursor.saturating_sub(1),
            KeyCode::Char('j') if !typing => p.cursor = (p.cursor + 1).min(n.saturating_sub(1)),
            KeyCode::Char('k') if !typing => p.cursor = p.cursor.saturating_sub(1),
            KeyCode::Backspace if typing && p.query.is_empty() => {
                (p.stage, p.cursor) = (ScopeStage::Top, 0)
            }
            KeyCode::Backspace if typing => {
                p.query.pop();
                p.cursor = 0;
            }
            KeyCode::Char(c) if typing && !k.modifiers.contains(KeyModifiers::CONTROL) => {
                p.query.push(c);
                p.cursor = 0;
            }
            KeyCode::Enter => {
                let choices = self.scope_choices(&p);
                match choices.get(p.cursor).map(|c| c.1.clone()) {
                    Some(ScopeChoice::Set(s)) => {
                        self.set_scope(s);
                        return None;
                    }
                    Some(ScopeChoice::Stage(st)) => {
                        (p.stage, p.cursor, p.query) = (st, 0, String::new());
                        match st {
                            ScopeStage::Orgs => self.fetch_orgs(),
                            ScopeStage::Repos => p.names = self.scope_repo_names(),
                            ScopeStage::Top => {}
                        }
                    }
                    None => {}
                }
            }
            _ => {}
        }
        Some(Modal::Scope(p))
    }

    /// `Enter` / `S` on a Repos row: that repo becomes the app's repo.
    fn open_repo_row(&mut self) {
        if let Some(r) = self.selected_in(self.focus).map(|i| i.repo.clone()) {
            self.go_repo(r);
        }
    }

    /// `f` on a Repos row: add or remove the favorite (the same list the repo browser edits).
    fn toggle_favorite(&mut self) {
        let Some(r) = self.selected_in(self.focus).map(|i| i.repo.clone()) else {
            return;
        };
        let before = self.cfg.repos.clone();
        let on = self.cfg.repos.toggle_fav(&r);
        if !self.save_cfg() {
            self.cfg.repos = before; // refused: keep memory and file in agreement
            return;
        }
        let (verb, prep) = if on {
            ("added", "to")
        } else {
            ("removed", "from")
        };
        self.status = format!("{verb} {r} {prep} favorites");
        self.refresh_repos_panels();
        if self.scope == Scope::Favorites {
            let s = self.scope.clone();
            self.set_scope(s);
        }
    }

    /// `H` on a Repos row or a row of a global section: hide or unhide its repo everywhere.
    fn toggle_hide(&mut self) {
        let Some(r) = self.selected_in(self.focus).map(|i| i.repo.clone()) else {
            return;
        };
        let before = self.cfg.repos.clone();
        let on = self.cfg.repos.toggle_hidden(&r);
        if !self.save_cfg() {
            self.cfg.repos = before;
            return;
        }
        self.status = format!("{} {r}", if on { "hidden:" } else { "unhidden:" });
        self.hidden_changed(&r, on);
    }

    /// The hidden list changed (here or in the repo browser): loaded sections lose a newly hidden
    /// repo's rows in memory (no search spent). An unhidden repo's rows were filtered out of what we
    /// hold, so the sections are searched again - the focused one now, the rest when focused - and
    /// the quota check in `load_global` decides whether that may happen yet.
    fn hidden_changed(&mut self, r: &str, now_hidden: bool) {
        if now_hidden {
            let drop_rows = |panels: &mut Vec<Panel>| {
                for p in panels.iter_mut().filter(|p| p.kind.is_global_search()) {
                    let before = p.items.len();
                    p.items.retain(|i| !i.repo.eq_ignore_ascii_case(r));
                    p.hidden += before - p.items.len();
                    p.cursor = p.cursor.min(p.items.len().saturating_sub(1));
                }
            };
            drop_rows(&mut self.panels);
            if let Some(o) = self.other.as_mut() {
                drop_rows(&mut o.panels);
            }
            self.reset_view();
        } else {
            self.stale_sections(true);
        }
    }

    fn refresh_repos_panels(&mut self) {
        for i in 0..self.panels.len() {
            if self.panels[i].kind == PK::Repos {
                self.load_repos_panel(i);
            }
        }
    }

    /// The `s` scope narrows panel `i` (always, for the built-in sections).
    pub fn scope_applies_to(&self, i: usize) -> bool {
        self.panels[i]
            .kind
            .custom()
            .is_none_or(|k| self.global && scope_applies(self.cfg.sections.get(k)))
    }

    /// The favorites scope is on but there is nothing to search for.
    pub fn favorites_missing(&self) -> bool {
        self.scope == Scope::Favorites
            && !self
                .cfg
                .repos
                .favorites
                .iter()
                .any(|f| crate::state::valid_repo(f))
    }

    /// The selected row is a repo in the Repos panel (list focus).
    /// Keys inside the `?` menu: j/k move, `/` filters, Esc clears the filter and then closes.
    fn help_key(&mut self, k: KeyEvent) {
        let max = self.help_max.get().saturating_sub(1);
        let close = |a: &mut App| {
            (a.help, a.help_typing) = (false, false);
            (a.help_scroll, a.help_filter) = (0, String::new());
            a.help_top.set(0);
        };
        match k.code {
            KeyCode::Down => self.help_scroll = (self.help_scroll + 1).min(max),
            KeyCode::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
            KeyCode::PageDown => self.help_scroll = (self.help_scroll + 10).min(max),
            KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
            KeyCode::Home => self.help_scroll = 0,
            KeyCode::End => self.help_scroll = max,
            KeyCode::Esc if self.help_typing => {
                (self.help_typing, self.help_filter, self.help_scroll) = (false, String::new(), 0)
            }
            KeyCode::Esc if !self.help_filter.is_empty() => {
                (self.help_filter, self.help_scroll) = (String::new(), 0)
            }
            KeyCode::Esc => close(self),
            KeyCode::Enter if self.help_typing => self.help_typing = false,
            KeyCode::Backspace if self.help_typing => {
                self.help_filter.pop();
                self.help_scroll = 0;
            }
            KeyCode::Char(c) if self.help_typing => {
                self.help_filter.push(c);
                self.help_scroll = 0;
            }
            KeyCode::Char('/') => self.help_typing = true,
            KeyCode::Char('j') => self.help_scroll = (self.help_scroll + 1).min(max),
            KeyCode::Char('k') => self.help_scroll = self.help_scroll.saturating_sub(1),
            KeyCode::Char('g') => self.help_scroll = 0,
            KeyCode::Char('G') => self.help_scroll = max,
            KeyCode::Char('q') => close(self),
            _ if self.keys.get(&k) == Some(Act::Help) => close(self),
            _ => {}
        }
    }

    /// A row of a global search section (Review, My PRs, Issues, custom...): its repo can be hidden.
    fn on_global_row(&self) -> bool {
        self.global
            && self.panels[self.focus].kind.is_global_search()
            && !self.detail_focus
            && self.ctx.is_none()
            && self.selected_in(self.focus).is_some()
    }

    fn on_repo_row(&self) -> bool {
        self.panels[self.focus].kind == PK::Repos
            && !self.detail_focus
            && self.ctx.is_none()
            && self.selected_in(self.focus).is_some()
    }

    /// `r`: just the selected item (its cached details) and the list tab it sits in.
    fn refresh_selected(&mut self) {
        // a search for this section is already out: asking again would only spend the minute's budget
        let p = &self.panels[self.focus];
        if self.ctx.is_none() && p.kind.is_global_search() && p.loading {
            self.status = "already searching".into();
            return;
        }
        let key = self.selected().map(Item::key);
        if let Some(k) = &key {
            self.cache.retain(|(ik, _), _| ik != k);
            self.dmeta.retain(|(ik, _), _| ik != k);
            self.revalidating.retain(|(ik, _)| ik != k);
            self.item_floor.insert(k.clone(), rate::now());
        }
        let k = self.panels[self.focus].kind;
        // first, so a refusal from the load (search quota low) is what the status line ends up saying
        self.status = match key {
            Some(_) => "refreshing the selected item".into(),
            None => "refreshing the list".into(),
        };
        if self.ctx.is_none()
            && (self.panels[self.focus].source().is_some()
                || k.is_global_search()
                || k == PK::Repos)
        {
            self.load_panel(self.focus, true);
        }
    }

    /// False when the save was refused or failed; the status line then says why.
    fn save_cfg(&mut self) -> bool {
        let Some(path) = &self.cfg_path else {
            return true;
        };
        match config::save_repos(path, &self.cfg.repos) {
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
            Modal::Scope(p) => self.scope_key(p, k),
            Modal::Menu(mut items, i) => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => None,
                KeyCode::Enter => self.pick(items.swap_remove(i)),
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
            Modal::Form(mut f) => match f.key(k) {
                form::Out::Cancel => None,
                form::Out::Picked("template") => {
                    f.apply_template();
                    Some(Modal::Form(f))
                }
                form::Out::Submit => match f.build() {
                    Ok(b) => {
                        let mut c = Confirm::new(b.argv, None);
                        c.stdin = b.stdin;
                        c.then = match f.spec {
                            form::Spec::Issue => Then::Select(PK::Issues),
                            form::Spec::Pr { .. } => Then::Select(PK::Prs),
                            form::Spec::Dispatch { .. } => Then::None,
                        };
                        c.back = Some(f);
                        Some(Modal::Confirm(c))
                    }
                    Err(e) => {
                        f.error = Some(e);
                        Some(Modal::Form(f))
                    }
                },
                _ => Some(Modal::Form(f)),
            },
            Modal::Confirm(mut c) => match k.code {
                // the whole command must have been on screen before it can run
                KeyCode::Char('y') if c.scroll < c.max_scroll.get() => {
                    self.status = "scroll to the end of the command first (G)".into();
                    Some(Modal::Confirm(c))
                }
                KeyCode::Char('y') => {
                    self.status = format!("running: {}", gh::shell(&c.cmd));
                    let tx = self.tx.clone();
                    self.mutation_job(
                        move || {
                            let r = match &c.local_of {
                                Some(repo) => gh::require_clone(repo)
                                    .and_then(|()| gh::run_with(&c.cmd, c.stdin.as_deref())),
                                None => gh::run_with(&c.cmd, c.stdin.as_deref()),
                            };
                            let _ = tx.send(Msg::Done(r, c.then));
                        },
                        Msg::Done(Err("the command failed unexpectedly".into()), Then::None),
                    );
                    None
                }
                KeyCode::Char('n') | KeyCode::Esc => c.back.map(Modal::Form),
                _ => {
                    let page = c.view_h.get().max(2) - 1;
                    c.scroll = match k.code {
                        _ if down => c.scroll.saturating_add(1),
                        _ if up => c.scroll.saturating_sub(1),
                        KeyCode::PageDown | KeyCode::Char(' ') => c.scroll.saturating_add(page),
                        KeyCode::PageUp => c.scroll.saturating_sub(page),
                        KeyCode::Home | KeyCode::Char('g') => 0,
                        KeyCode::End | KeyCode::Char('G') => u16::MAX,
                        _ => c.scroll,
                    }
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
                    f.raw_path.clone(),
                    fd.pr_id.clone(),
                )
            }),
            _ => None,
        };
        let Some((key, sha, path, raw_path, pr_id)) = target else {
            return;
        };
        match self.viewed.toggle(&key, &sha, &path) {
            Err(e) => self.status = e,
            // `sync_viewed = true` in config.toml is the consent for this GitHub write
            Ok(now_on) if self.cfg.sync_viewed && !pr_id.is_empty() => {
                let (tx, cmd) = (
                    self.tx.clone(),
                    gh::viewed_mutation(&pr_id, &raw_path, now_on),
                );
                self.mutation_job(
                    move || {
                        let r = gh::run_with(&cmd, None)
                            .map(|_| {
                                format!(
                                    "{} on GitHub",
                                    if now_on { "marked viewed" } else { "unmarked" }
                                )
                            })
                            .map_err(|e| {
                                format!("GitHub viewed sync failed (local mark kept): {e}")
                            });
                        let _ = tx.send(Msg::Status(r));
                    },
                    Msg::Status(Err("GitHub viewed sync failed (local mark kept)".into())),
                );
            }
            Ok(_) => {}
        }
    }

    /// Drill into the selected PR: the left column becomes its Files / Checks / Comments.
    fn enter_ctx(&mut self) {
        let Some(pr) = self.pr_item().cloned() else {
            return;
        };
        let ctx_panels = vec![
            Panel::new(PK::Files, "Files", vec![]),
            Panel::new(PK::Commits, "Commits", vec![]),
            Panel::new(PK::Checks, "Checks", vec![]),
            Panel::new(PK::Comments, "Comments", vec![]),
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
        let opened = !set.remove(&key);
        if opened {
            set.insert(key);
        }
        if reactions && opened {
            self.fetch_reactors();
        }
    }

    /// `e` opened the reaction names of the selected comment: the first page only has counts, so ask
    /// for the names (one small call per comment that lacks them).
    fn fetch_reactors(&mut self) {
        let Some(item) = self.selected().map(Item::key) else {
            return;
        };
        let ids = self
            .comments_data()
            .and_then(|cd| cd.get(self.lrow()))
            .map(|e| e.card.nameless_ids())
            .unwrap_or_default();
        for id in ids {
            if !self.react_pending.insert(id.clone()) {
                continue;
            }
            let (tx, g, item) = (self.tx.clone(), self.dgen, item.clone());
            let (tx2, id2, item2) = (tx.clone(), id.clone(), item.clone());
            self.user_job_or(
                move || {
                    let r = gh::reactors(&id);
                    let _ = tx.send(Msg::Reactors(item, id, g, r));
                },
                // lost: clear the "in flight" mark so `e` can ask again
                move || {
                    let _ = tx2.send(Msg::Reactors(item2, id2, g, Err("request lost".into())));
                },
            );
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
        if self.on_repo_row() {
            return self.open_repo_row();
        }
        if !self.detail_focus {
            if self.ctx.is_none() && self.panels[self.focus].kind.is_pr_list() {
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
        self.user_job(move || {
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
        if let Some(url) = self.selected().map(|i| i.url.clone()) {
            self.open_url(url);
        }
    }

    fn open_url(&mut self, url: String) {
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
                Ok(s) if s.success() => Ok(format!("opened {url}")),
                _ => Err(format!("could not open {url}")),
            }
        });
    }

    fn copy(&mut self) {
        // a tag has no page of its own worth pasting: copy its name
        let Some(url) = self.selected().map(|i| {
            if i.kind == Kind::Tag {
                i.cmd_name().to_string()
            } else {
                i.url.clone()
            }
        }) else {
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
            Some(it) if !it.repo_ok() => {
                self.status = "this repo's name looks unsafe: not checking it out".into()
            }
            Some(it) if it.kind == Kind::Pr => {
                let (repo, n) = (it.repo.clone(), it.number);
                self.bg(move || gh::checkout(&repo, n));
            }
            Some(it) if it.kind == Kind::Tag => {
                self.status = "checkout is not available for tags (copy the name with y)".into()
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
        let dir = std::env::temp_dir().join(format!("gh-tui-app-test-{}", std::process::id()));
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

    #[test]
    fn github_viewed_marks_are_merged_into_the_local_ones() {
        let mut a = App::with(
            "o/r".into(),
            Theme::new(false, IconSet::Unicode, true),
            false,
        );
        let fd = FilesData {
            sha: "s1".into(),
            viewed_key: Viewed::key("o/r", 7),
            gh_viewed: vec!["src/a.rs".into()],
            pr_id: "PR_1".into(),
            ..Default::default()
        };
        a.tx.send(Msg::Detail(
            "k".into(),
            Tab::Diff,
            a.dgen,
            Ok(Data::Files(fd)),
        ))
        .unwrap();
        a.poll();
        assert!(a.viewed.is_viewed(&Viewed::key("o/r", 7), "s1", "src/a.rs"));
        assert!(!a.viewed.is_viewed(&Viewed::key("o/r", 7), "s1", "src/b.rs"));
    }

    #[test]
    fn late_or_stale_state_is_dropped_on_repo_switch() {
        let mut a = App::with("o/r".into(), Theme::new(false, IconSet::Ascii, true), false);
        a.cwd_branch = Some("feat".into());
        a.pending_select = Some((PK::Prs, 7, "o/r".into()));
        a.switch_repo("x/y".into());
        assert!(a.cwd_branch.is_none() && a.pending_select.is_none());
        let stale = a.hgen - 1;
        a.tx.send(Msg::Branch("o/r".into(), Some("old".into())))
            .unwrap();
        a.tx.send(Msg::User(stale, "mallory".into())).unwrap();
        a.poll();
        assert!(
            a.cwd_branch.is_none() && !a.header.contains("mallory"),
            "a reply for the repo we left"
        );
        a.tx.send(Msg::Branch("x/y".into(), Some("new".into())))
            .unwrap();
        a.poll();
        assert_eq!(a.cwd_branch.as_deref(), Some("new"));
    }

    #[test]
    fn paste_goes_to_the_focused_text_box_only() {
        let mut a = App::with("o/r".into(), Theme::new(false, IconSet::Ascii, true), false);
        a.on_paste("ignored");
        assert!(a.filter.is_empty(), "no text box focused");
        a.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        a.on_paste("a\nb\x1b");
        assert_eq!(a.filter, "ab");
        a.modal = Some(Modal::Input {
            title: "t",
            buf: String::new(),
            required: false,
            build: Box::new(|_| vec![]),
        });
        a.on_paste("x\r\ny");
        assert!(matches!(&a.modal, Some(Modal::Input { buf, .. }) if buf == "x\ny"));
    }

    fn plain() -> App {
        App::with(
            "o/r".into(),
            Theme::new(false, IconSet::Unicode, true),
            false,
        )
    }

    fn pr(n: u64) -> Item {
        Item {
            number: n,
            title: format!("pr {n}"),
            repo: "o/r".into(),
            kind: Kind::Pr,
            ..Default::default()
        }
    }

    #[test]
    fn inbox_enter_shows_the_item_here_after_one_reload() {
        let mut a = plain();
        let mut n = pr(7);
        n.cmd = "55".into();
        a.seed_inbox(vec![n]);
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(a.inbox.is_none());
        assert_eq!(a.pending_select, Some((PK::Prs, 7, "o/r".into())));
        let p = &a.panels[0];
        assert_eq!(p.tab_id(), 2, "looks in All open");
        let (tab, seq) = (p.tab, p.seq);
        a.tx.send(Msg::List(PK::Prs, tab, seq, Ok(vec![pr(3), pr(7)])))
            .unwrap();
        a.poll();
        assert_eq!((a.panels[0].cursor, a.focus), (1, 0));
        assert!(a.pending_select.is_none());
        assert_eq!(
            a.counts.get(&(PK::Prs, 2)),
            Some(&(2, false)),
            "the list doubles as its count"
        );
    }

    #[test]
    fn inbox_enter_switches_repo_and_reports_a_missing_item() {
        let mut a = plain();
        let mut n = pr(9);
        n.repo = "x/y".into();
        a.seed_inbox(vec![n]);
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(a.repo, "x/y");
        assert_eq!(a.pending_select, Some((PK::Prs, 9, "x/y".into())));
        let (tab, seq) = (a.panels[0].tab, a.panels[0].seq);
        a.tx.send(Msg::List(PK::Prs, tab, seq, Ok(vec![pr(1)])))
            .unwrap();
        a.poll();
        assert!(
            a.pending_select.is_none() && a.status.contains("#9 is not open"),
            "{}",
            a.status
        );
    }

    #[test]
    fn inbox_enter_needs_the_panel_it_opens_in() {
        let cfg = config::parse("[panels]\nshow = [\"issues\"]\n", "t").unwrap();
        let mut a = App::build(
            "o/r".into(),
            Theme::new(false, IconSet::Unicode, true),
            false,
            cfg,
        );
        a.seed_inbox(vec![pr(7)]);
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            a.inbox.is_some() && a.status.contains("hidden in [panels]"),
            "{}",
            a.status
        );
    }

    #[test]
    fn mark_read_refuses_odd_thread_ids() {
        let mut a = plain();
        let mut n = pr(7);
        n.cmd = "../x".into();
        a.seed_inbox(vec![n]);
        a.on_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        assert!(a.modal.is_none() && a.status.contains("nothing to mark read"));
    }

    #[test]
    fn late_counts_from_before_a_reload_are_dropped() {
        let mut a = plain();
        let old = a.cgen;
        a.reload_all();
        a.tx.send(Msg::Count(old, (PK::Repo, 1), Ok((5, false))))
            .unwrap();
        a.tx.send(Msg::Count(a.cgen, (PK::Repo, 2), Ok((9, true))))
            .unwrap();
        a.poll();
        assert_eq!(a.counts.get(&(PK::Repo, 1)), None);
        assert_eq!(a.counts.get(&(PK::Repo, 2)), Some(&(9, true)));
    }

    /// An app that really runs `gh` (the shim) and has been idle for a long time.
    fn live(toml: &str) -> App {
        let cfg = config::parse(toml, "t").unwrap();
        let mut a = App::build(
            "o/r".into(),
            Theme::new(false, IconSet::Unicode, true),
            false,
            cfg,
        );
        a.net = true;
        a.last_input = std::time::Instant::now() - std::time::Duration::from_secs(60);
        a
    }

    fn wait(a: &mut App, what: &str, ok: impl Fn(&App) -> bool) {
        // a generous deadline (30 s): it only matters when something is really wrong
        for _ in 0..15000 {
            a.poll();
            if ok(a) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("timed out waiting for {what}");
    }

    /// Waits for queued and running jobs to finish, so a "nothing was fetched" check sees every call.
    fn quiesce() {
        for _ in 0..30000 {
            if crate::pool::global().idle() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("pool never went idle");
    }

    fn list_calls(shim: &crate::testshim::Shim) -> Vec<String> {
        shim.calls()
            .into_iter()
            .filter(|c| {
                c.contains(" list") || c.starts_with("api repos") || c.contains("api --cache")
            })
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn low_quota_shows_the_chip_then_pauses_background_work_and_recovers() {
        let shim = crate::testshim::Shim::new();
        let later = rate::now() + 3600;
        shim.set("rate.out", &crate::testshim::rate_doc(900, later));
        let mut a = live("[api]\ncounts = \"eager\"\n");
        a.spawn_rate_poll();
        wait(&mut a, "the quota poll", |a| a.rate.graphql.is_some());
        // 18%: low (chip) but not paused
        let chip = a.quota_chip().expect("chip under 20%");
        assert_eq!((chip.remaining, chip.limit), (900, 5000));
        assert!(a.quota_low && !a.paused && a.pause_note().is_none());
        a.ensure();
        assert!(a.cpending.is_empty(), "low quota: no background counts");
        // 6%: paused, with the note and when it ends
        shim.set("rate.out", &crate::testshim::rate_doc(300, later));
        a.spawn_rate_poll();
        wait(&mut a, "pause", |a| a.paused);
        let note = a.pause_note().unwrap();
        assert!(
            note.starts_with("paused background refresh (rate limit low, resets "),
            "{note}"
        );
        a.ensure();
        assert!(a.cpending.is_empty());
        assert!(
            list_calls(&shim).is_empty(),
            "nothing fetched in the background: {:?}",
            shim.calls()
        );
        // user work still runs while paused
        a.load_panel(2, false);
        wait(&mut a, "a user-initiated list", |a| !a.panels[2].loading);
        assert!(!list_calls(&shim).is_empty());
        // the quota is back: the pause ends and counts resume
        shim.set("rate.out", &crate::testshim::rate_doc(5000, later));
        a.spawn_rate_poll();
        wait(&mut a, "recovery", |a| !a.paused && !a.quota_low);
        assert!(a.pause_note().is_none() && a.quota_chip().is_none());
        a.ensure();
        wait(&mut a, "a count", |a| !a.counts.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_secondary_limit_backs_background_work_off_for_the_time_it_names() {
        let shim = crate::testshim::Shim::new();
        shim.set(
            "graphql.err",
            "You have exceeded a secondary rate limit. Please wait a few minutes. Retry-After: 30",
        );
        let err = gh::graphql(vec!["api".into(), "graphql".into()]).unwrap_err();
        assert!(err.contains("secondary rate limit"), "{err}");
        let until = rate::snapshot().backoff_until;
        let now = rate::now();
        assert!(
            (now + 25..=now + 31).contains(&until),
            "backs off for the 30s it was told: {until} vs {now}"
        );
        let mut a = live("[api]\ncounts = \"eager\"\n");
        a.poll();
        assert!(
            a.paused && !a.quota_low,
            "backing off pauses without a chip"
        );
        a.ensure();
        assert!(a.cpending.is_empty());
        let work = |s: &crate::testshim::Shim| {
            s.calls()
                .iter()
                .filter(|c| !c.contains("rate_limit"))
                .count()
        };
        let before = work(&shim);
        for _ in 0..5 {
            a.ensure();
        }
        quiesce();
        assert_eq!(work(&shim), before, "no retry loop while backing off");
        // once the time has passed everything resumes
        rate::reset_for_test();
        a.poll();
        assert!(!a.paused);
    }

    #[cfg(unix)]
    #[test]
    fn tab_counts_are_lazy_and_never_touch_search_backed_tabs() {
        let shim = crate::testshim::Shim::new();
        shim.set("prlist.out", "[]");
        shim.set("issuelist.out", "[]");
        let mut a = live("");
        // PRs focused: Mine and Review are search-backed (`?`), All and Merged are plain lists
        a.ensure();
        wait(&mut a, "first count", |a| !a.counts.is_empty());
        for _ in 0..3 {
            a.ensure();
            wait(&mut a, "no pending", |a| a.cpending.is_empty());
        }
        let keys: std::collections::HashSet<_> = a.counts.keys().copied().collect();
        assert_eq!(keys, [(PK::Prs, 2), (PK::Prs, 3)].into(), "{keys:?}");
        let c = shim.calls();
        assert!(
            c.iter()
                .all(|c| !c.contains("--author") && !c.contains("--search")),
            "{c:?}"
        );
        assert!(a.count_wanted(0, 2) && !a.count_wanted(0, 0) && !a.count_wanted(0, 1));
        // other panels wait until focused
        assert!(!a.counts.keys().any(|k| k.0 != PK::Prs));
        // not for a second after typing
        a.last_input = std::time::Instant::now();
        a.set_focus(2);
        a.ensure();
        assert!(
            a.cpending.is_empty(),
            "lazy counts wait for a second of idling"
        );
        // global view and "off" fetch nothing
        a.last_input = std::time::Instant::now() - std::time::Duration::from_secs(60);
        a.global = true;
        a.ensure();
        assert!(a.cpending.is_empty());
        a.global = false;
        let mut off = live("[api]\ncounts = \"off\"\n");
        off.ensure();
        assert!(off.cpending.is_empty() && !off.count_wanted(0, 2));
    }

    #[cfg(unix)]
    #[test]
    fn eager_counts_cover_every_panel() {
        let shim = crate::testshim::Shim::new();
        shim.set("prlist.out", "[]");
        shim.set("issuelist.out", "[]");
        let mut a = live("[api]\ncounts = \"eager\"\n");
        for _ in 0..400 {
            a.ensure();
            a.poll();
            if a.counts.contains_key(&(PK::Issues, 2)) && a.counts.contains_key(&(PK::Repo, 2)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(a.counts.contains_key(&(PK::Issues, 2)) && a.counts.contains_key(&(PK::Repo, 2)));
        assert!(
            !a.counts.contains_key(&(PK::Prs, 0)),
            "search-backed tabs stay `?`"
        );
        let _ = shim;
    }

    #[test]
    fn expensive_tabs_wait_longer_than_the_overview() {
        let mut a = plain();
        assert_eq!(a.settle(Tab::Overview).as_millis(), 100);
        for t in [Tab::Comments, Tab::Diff, Tab::Commits, Tab::Checks] {
            assert_eq!(a.settle(t).as_millis(), 250, "{t:?}");
        }
        a.cfg.api.settle_ms = 400;
        assert_eq!(a.settle(Tab::Diff).as_millis(), 400);
        assert_eq!(a.settle(Tab::Overview).as_millis(), 100);
    }

    #[cfg(unix)]
    #[test]
    fn a_detail_fetch_starts_only_after_its_settle_delay() {
        let shim = crate::testshim::Shim::new();
        let mut a = live("");
        a.panels[0].items = vec![pr(7)];
        let it = pr(7);
        // a settle delay no test run can outlast, so "not yet" is exact; the Overview's is fixed (100 ms)
        a.cfg.api.settle_ms = 3_600_000;
        a.last_input = std::time::Instant::now() - std::time::Duration::from_secs(5);
        a.fetch(it.clone(), Tab::Diff);
        assert!(
            a.cache.is_empty(),
            "long enough for an Overview, not for a diff"
        );
        a.fetch(it.clone(), Tab::Overview);
        assert!(
            a.cache.is_empty(),
            "the Overview waits for the item to rest a whole tick"
        );
        a.want_prev = a.want_now.clone();
        a.fetch(it.clone(), Tab::Overview);
        assert_eq!(a.cache.len(), 1);
        a.cfg.api.settle_ms = 0;
        a.fetch(it, Tab::Diff);
        assert_eq!(a.cache.len(), 2);
        wait(&mut a, "details", |a| {
            a.cache.values().all(|l| matches!(l, Load::Done(_)))
        });
        let _ = shim;
    }

    const OVERVIEW: &str = r#"{"isDraft":false,"mergeable":"MERGEABLE","baseRefName":"main","headRefName":"x","reviewDecision":null,"reviewRequests":[],"latestReviews":[]}"#;

    /// Asks for `tab` of `it` the way an idle tick does, and waits until it is answered.
    fn get_detail(a: &mut App, it: &Item, tab: Tab) {
        a.cfg.api.settle_ms = 0;
        a.last_input = std::time::Instant::now() - std::time::Duration::from_secs(60);
        a.want_prev.insert((it.key(), tab));
        a.fetch(it.clone(), tab);
        let k = (it.key(), tab);
        wait(a, "a detail", |a| {
            matches!(a.cache.get(&k), Some(Load::Done(_))) && !a.revalidating.contains(&k)
        });
    }

    fn pr_views(shim: &crate::testshim::Shim) -> usize {
        shim.calls()
            .iter()
            .filter(|c| c.starts_with("pr view"))
            .count()
    }

    #[cfg(unix)]
    #[test]
    fn details_survive_a_restart_until_the_item_is_updated() {
        let shim = crate::testshim::Shim::new();
        shim.set("rest.out", OVERVIEW);
        let mut it = pr(7);
        it.updated = "2026-10-03T10:00:00Z".into();
        let mut a = live("");
        get_detail(&mut a, &it, Tab::Overview);
        assert_eq!(pr_views(&shim), 1);
        // a new run: the copy on disk is still good, so GitHub is not asked again
        let mut b = live("");
        get_detail(&mut b, &it, Tab::Overview);
        assert!(matches!(
            b.data_of(&it, Tab::Overview),
            Some(Load::Done(Ok(_)))
        ));
        assert_eq!(pr_views(&shim), 1, "served from disk: {:?}", shim.calls());
        // the list now shows the PR as updated: the stored copy is stale, so it is shown and refreshed
        let mut newer = it.clone();
        newer.updated = "2026-10-03T12:00:00Z".into();
        let mut c = live("");
        get_detail(&mut c, &newer, Tab::Overview);
        assert_eq!(pr_views(&shim), 2, "{:?}", shim.calls());
        assert_eq!(
            c.dmeta
                .get(&(newer.key(), Tab::Overview))
                .map(|m| m.stamp.as_str()),
            Some("2026-10-03T12:00:00Z")
        );
        // [cache] details = false: nothing is read from or written to disk
        let mut d = live("[cache]\ndetails = false\n");
        let mut other = pr(8);
        other.updated = "2026-10-03T10:00:00Z".into();
        get_detail(&mut d, &other, Tab::Overview);
        let mut e = live("[cache]\ndetails = false\n");
        get_detail(&mut e, &other, Tab::Overview);
        assert_eq!(
            pr_views(&shim),
            4,
            "no disk copy to reuse: {:?}",
            shim.calls()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_session_refreshes_a_detail_when_the_list_shows_the_item_updated() {
        let shim = crate::testshim::Shim::new();
        shim.set("rest.out", OVERVIEW);
        let mut it = pr(7);
        it.updated = "2026-10-03T10:00:00Z".into();
        let mut a = live("");
        get_detail(&mut a, &it, Tab::Overview);
        // unchanged: another tick asks for nothing
        get_detail(&mut a, &it, Tab::Overview);
        assert_eq!(pr_views(&shim), 1);
        // a teammate commented: the next list fetch shows a newer updatedAt, so the old copy stays on
        // screen while a new one is fetched
        it.updated = "2026-10-03T11:00:00Z".into();
        a.fetch(it.clone(), Tab::Overview);
        assert!(a.revalidating.contains(&(it.key(), Tab::Overview)));
        assert!(
            matches!(a.data_of(&it, Tab::Overview), Some(Load::Done(Ok(_)))),
            "old copy kept"
        );
        get_detail(&mut a, &it, Tab::Overview);
        assert_eq!(pr_views(&shim), 2);
        // a failed refresh keeps the copy and waits before trying again
        it.updated = "2026-10-03T12:00:00Z".into();
        shim.set("rest.err", "boom");
        get_detail(&mut a, &it, Tab::Overview);
        assert!(matches!(
            a.data_of(&it, Tab::Overview),
            Some(Load::Done(Ok(_)))
        ));
        assert!(a.status.contains("refresh failed"), "{}", a.status);
        let n = pr_views(&shim);
        a.fetch(it.clone(), Tab::Overview);
        quiesce();
        assert_eq!(pr_views(&shim), n, "held for a minute after a failure");
    }

    #[test]
    fn a_dropped_detail_fetch_forgets_its_placeholder() {
        let mut a = plain();
        let it = pr(7);
        let key = (it.key(), Tab::Diff);
        a.cache.insert(key.clone(), Load::Loading);
        a.tx.send(Msg::Dropped(it.key(), Tab::Diff, a.dgen + 1))
            .unwrap();
        a.poll();
        assert!(
            a.cache.contains_key(&key),
            "a drop from an older generation is ignored"
        );
        a.tx.send(Msg::Dropped(it.key(), Tab::Diff, a.dgen))
            .unwrap();
        a.poll();
        assert!(!a.cache.contains_key(&key), "so it is asked for again");
    }

    #[test]
    fn r_refreshes_the_selected_item_only_and_capital_r_everything() {
        let mut a = plain();
        a.panels[0].items = vec![pr(7), pr(8)];
        for n in [7, 8] {
            for t in [Tab::Overview, Tab::Comments] {
                a.cache
                    .insert((pr(n).key(), t), Load::Done(Ok(Data::Text(vec![]))));
            }
        }
        let g = a.dgen;
        a.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(a.cache.len(), 2, "only #7's entries went");
        assert!(a.cache.keys().all(|(k, _)| *k == pr(8).key()));
        assert_eq!(a.dgen, g, "no global invalidation");
        assert!(a.panels[0].loading, "its list tab reloads");
        a.on_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE));
        assert!(a.cache.is_empty() && a.dgen > g, "R reloads everything");
    }

    #[cfg(unix)]
    #[test]
    fn startup_is_one_graphql_call_and_the_facts_are_cached() {
        let shim = crate::testshim::Shim::new();
        shim.set("graphql.out", include_str!("../tests/startup.json"));
        let mut a = live("");
        a.start_load(false);
        wait(&mut a, "the lists", |a| {
            !a.panels[0].loading && !a.panels[2].loading
        });
        wait(&mut a, "the facts", |a| {
            a.meta.is_some() && !a.user.is_empty()
        });
        assert_eq!(shim.calls().len(), 1, "{:?}", shim.calls());
        assert_eq!(a.panels[0].items.len(), 2);
        assert_eq!(a.panels[2].items[0].number, 3);
        assert_eq!(a.counts.get(&(PK::Prs, 2)), Some(&(74, false)));
        assert_eq!(a.counts.get(&(PK::Prs, 0)), Some(&(2, false)));
        assert!(a.header.contains("user: octocat"));
        // a second start while the copies are fresh asks GitHub for nothing: lists, facts and the
        // viewer all come from disk
        let mut b = live("");
        b.start_load(false);
        assert!(
            !b.panels[0].loading && b.panels[0].items.len() == 2,
            "shown at once"
        );
        assert!(b.meta.is_some(), "the cached facts show at once");
        assert!(b.header.contains("user: octocat"), "{}", b.header);
        assert_eq!(shim.calls().len(), 1, "{:?}", shim.calls());
        // r/R go to GitHub, facts included
        b.start_load(true);
        wait(&mut b, "lists", |a| !a.panels[0].loading);
        assert!(
            shim.calls()[1].contains("-F meta=true"),
            "{:?}",
            shim.calls()
        );
    }

    fn cached_rows(kind: Kind, repo: &str, n: u64) -> Vec<Item> {
        (1..=n)
            .map(|i| Item {
                number: i,
                title: format!("cached {i}"),
                url: format!("https://github.com/{repo}/pull/{i}"),
                repo: repo.into(),
                kind,
                updated: "2026-10-03T10:00:00Z".into(),
                ..Default::default()
            })
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_repo_list_shows_at_once_marked_and_is_refreshed() {
        let shim = crate::testshim::Shim::new();
        shim.set("graphql.out", include_str!("../tests/startup.json"));
        let mut a = live("");
        let key = a.list_key(PK::Prs, 0).unwrap();
        // older than hot_s (120 s): stale, but better than a blank while GitHub answers
        dcache::write_list(
            &key,
            &cached_rows(Kind::Pr, "o/r", 1),
            0,
            0,
            rate::now() - 1000,
        );
        a.start_load(false);
        let p = &a.panels[0];
        assert!(
            p.loading && p.items.len() == 1 && p.cached_at.is_some(),
            "shown, marked, refreshing"
        );
        wait(&mut a, "the refresh", |a| !a.panels[0].loading);
        assert!(a.panels[0].cached_at.is_none() && a.panels[0].items.len() == 2);
        assert_eq!(shim.calls().len(), 1, "one request, as before");
        // the refreshed list is on disk for the next run
        let l = dcache::read_list(&key, rate::now()).unwrap();
        assert_eq!(l.items.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn a_global_section_uses_a_fresh_copy_without_searching_and_refreshes_a_stale_one() {
        let shim = crate::testshim::Shim::new();
        shim.set("searchprs.out", "[]");
        let mut a = home();
        a.net = true;
        let key = a.list_key(PK::Review, 0).unwrap();
        dcache::write_list(
            &key,
            &cached_rows(Kind::Pr, "a/b", 3),
            2,
            0,
            rate::now() - 30,
        );
        a.load_panel(0, false);
        assert_eq!((a.panels[0].items.len(), a.panels[0].hidden), (3, 2));
        assert!(!a.panels[0].loading && a.panels[0].cached_at.is_none());
        assert_eq!(a.counts.get(&(PK::Review, 0)), Some(&(3, false)));
        assert!(
            shim.calls().is_empty(),
            "fresh: no search: {:?}",
            shim.calls()
        );
        // r searches regardless
        a.load_panel(0, true);
        wait(&mut a, "the search", |a| !a.panels[0].loading);
        assert_eq!(shim.calls().len(), 1);
        assert!(a.panels[0].items.is_empty(), "the answer replaced the copy");
        // a copy past hot_s is shown marked while the search runs
        dcache::write_list(
            &key,
            &cached_rows(Kind::Pr, "a/b", 3),
            0,
            0,
            rate::now() - 600,
        );
        a.load_panel(0, false);
        assert!(
            a.panels[0].loading && a.panels[0].items.len() == 3 && a.panels[0].cached_at.is_some()
        );
        wait(&mut a, "the refresh", |a| !a.panels[0].loading);
        assert_eq!(shim.calls().len(), 2);
        assert!(a.panels[0].cached_at.is_none());
        // a failed refresh keeps the copy and says so
        dcache::write_list(
            &key,
            &cached_rows(Kind::Pr, "a/b", 3),
            0,
            0,
            rate::now() - 600,
        );
        shim.set("searchprs.err", "boom");
        a.load_panel(0, false);
        wait(&mut a, "the failure", |a| !a.panels[0].loading);
        assert!(a.panels[0].error.is_none() && a.panels[0].items.len() == 3);
        assert!(a.status.contains("showing the cached list"), "{}", a.status);
        // another window, scope or hidden set is another list
        a.window = config::Window::Day;
        assert_ne!(a.list_key(PK::Review, 0).unwrap(), key);
        a.window = config::Window::Week;
        a.cfg.repos.hidden = vec!["x/y".into()];
        assert_ne!(a.list_key(PK::Review, 0).unwrap(), key);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_batch_falls_back_but_a_rate_limit_does_not() {
        let shim = crate::testshim::Shim::new();
        shim.set("graphql.err", "boom: schema changed");
        let mut a = live("");
        a.start_load(false);
        wait(&mut a, "fallback", |_| {
            let c = shim.calls();
            c.iter().any(|c| c.contains("pr list")) && c.iter().any(|c| c.contains("issue list"))
        });
        wait(&mut a, "done", |a| {
            !a.panels[0].loading && !a.panels[2].loading
        });
        shim.clear("graphql.err");
        // rate limit: show it, do not ask again the old way
        shim.set(
            "graphql.err",
            "API rate limit exceeded for user ID 1. Retry-After: 45",
        );
        let before = shim.calls().len();
        let mut b = live("");
        b.start_load(true);
        wait(&mut b, "error", |a| a.panels[0].error.is_some());
        assert!(b.panels[0].error.as_deref().unwrap().contains("rate limit"));
        assert!(
            shim.calls()[before..].iter().all(|c| !c.contains("list")),
            "{:?}",
            shim.calls()
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_repo_browser_shows_cached_rows_at_once_then_refreshes_stale_ones() {
        let shim = crate::testshim::Shim::new();
        let row = |n: &str| RepoRow {
            name: format!("o/{n}"),
            owner: "o".into(),
            ..Default::default()
        };
        let store = crate::cache::Store::default_if_enabled().unwrap();
        let key = repos_key("github.com", "octocat");
        let put = |who: &str, rows: Vec<RepoRow>, at: u64| {
            let c = CachedRepos {
                host: "github.com".into(),
                viewer: who.into(),
                rows,
                truncated: false,
            };
            store.write(&key, &c, at).unwrap();
        };
        let page = |n: &str| {
            format!(
                r#"{{"data":{{"viewer":{{"login":"octocat","repositories":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[{{"nameWithOwner":"o/{n}","isPrivate":false,"isFork":false,"isArchived":false,"stargazerCount":1,"pushedAt":"2026-01-01T00:00:00Z","primaryLanguage":null,"owner":{{"__typename":"User","login":"o"}},"issues":{{"totalCount":0}},"pullRequests":{{"totalCount":0}}}}]}}}}}}}}"#
            )
        };
        shim.set("graphql.out", &page("fresh"));
        // fresh cache: shown, no call at all
        put("octocat", vec![row("cached")], rate::now());
        let mut a = live("");
        a.open_browser(false);
        assert_eq!(
            a.browser.as_ref().unwrap().rows[0].name,
            "o/cached",
            "on screen at once"
        );
        wait(&mut a, "loading to end", |a| {
            !a.browser.as_ref().unwrap().loading
        });
        assert!(
            shim.calls().is_empty(),
            "within the TTL nothing is fetched: {:?}",
            shim.calls()
        );
        // stale cache: shown at once, then replaced
        put(
            "octocat",
            vec![row("cached")],
            rate::now() - a.cfg.cache.slow_s - 5,
        );
        a.open_browser(false);
        assert_eq!(a.browser.as_ref().unwrap().rows[0].name, "o/cached");
        wait(&mut a, "refreshed", |a| {
            a.browser
                .as_ref()
                .is_some_and(|b| !b.loading && b.rows[0].name == "o/fresh")
        });
        assert_eq!(
            a.browser.as_ref().unwrap().rows.len(),
            1,
            "replaced, not appended"
        );
        let (c, _) = store.read::<CachedRepos>(&key, rate::now()).unwrap();
        assert_eq!(c.rows[0].name, "o/fresh", "the new listing is cached");
        // r in the browser bypasses the cache
        let n = shim.calls().len();
        a.open_browser(true);
        assert!(
            a.browser.as_ref().unwrap().rows.is_empty(),
            "no cached rows on a bypass"
        );
        wait(&mut a, "bypass", |a| {
            a.browser
                .as_ref()
                .is_some_and(|b| !b.loading && !b.rows.is_empty())
        });
        assert!(shim.calls().len() > n);
    }

    #[cfg(unix)]
    #[test]
    fn another_account_or_host_never_sees_cached_data() {
        let shim = crate::testshim::Shim::new();
        shim.set("graphql.out", include_str!("../tests/startup.json"));
        let store = crate::cache::Store::default_if_enabled().unwrap();
        // alice (the shim's viewer is octocat): her facts and repo list are cached
        let mut a = live("");
        a.start_load(false);
        wait(&mut a, "alice's facts", |a| a.meta.is_some());
        let at = rate::now();
        let c = CachedRepos {
            host: "github.com".into(),
            viewer: "octocat".into(),
            rows: vec![RepoRow {
                name: "o/private-one".into(),
                ..Default::default()
            }],
            truncated: false,
        };
        store
            .write(&repos_key("github.com", "octocat"), &c, at)
            .unwrap();
        // the account switches to bob: nothing of alice's is used, nothing is written under bob
        gh::set_identity(Some(("github.com".into(), "bob".into())));
        let calls = shim.calls().len();
        let mut b = live("");
        b.start_load(false);
        wait(&mut b, "bob's startup", |b| !b.panels[0].loading);
        assert!(
            shim.calls()[calls].contains("-F meta=true"),
            "alice's facts were discarded: {:?}",
            shim.calls()
        );
        // the facts follow the lists as a message of their own: wait for them, don't race the job
        wait(&mut b, "bob's facts", |b| b.meta.is_some());
        b.open_browser(false);
        assert!(
            b.browser.as_ref().unwrap().rows.is_empty(),
            "alice's repo list stays hidden"
        );
        wait(&mut b, "listing", |b| {
            b.browser.as_ref().is_some_and(|x| !x.loading)
        });
        assert!(
            store
                .read::<CachedRepos>(&repos_key("github.com", "bob"), rate::now())
                .is_none(),
            "an answer for someone else is not stored under bob"
        );
        // another host: its own keys, so alice's entries do not apply
        gh::set_identity(Some(("ghe.example.com".into(), "octocat".into())));
        assert!(read_repos().is_none() && read_meta("o/r", 300).is_none());
        // back as alice everything is still there
        gh::set_identity(Some(("github.com".into(), "octocat".into())));
        assert_eq!(read_repos().unwrap().0.rows[0].name, "o/private-one");
        assert!(read_meta("o/r", 300).is_some());
        // an entry whose stored identity does not match its key is refused (tampering)
        let forged = CachedRepos {
            viewer: "mallory".into(),
            ..read_repos().unwrap().0
        };
        store
            .write(&repos_key("github.com", "octocat"), &forged, rate::now())
            .unwrap();
        assert!(read_repos().is_none());
        // no known login (a token from the environment): nothing user-specific is cached
        gh::set_identity(None);
        assert!(read_repos().is_none() && read_meta("o/r", 300).is_none());
    }

    #[test]
    fn text_read_back_from_disk_is_neutralized_again() {
        let _shim = crate::testshim::Shim::new();
        let store = crate::cache::Store::default_if_enabled().unwrap();
        let c = CachedRepos {
            host: "github.com".into(),
            viewer: "octocat".into(),
            rows: vec![RepoRow {
                name: "o/we\u{202e}ird".into(),
                owner: "o\u{1b}[2J".into(),
                ..Default::default()
            }],
            truncated: false,
        };
        store
            .write(&repos_key("github.com", "octocat"), &c, rate::now())
            .unwrap();
        let r = &read_repos().unwrap().0.rows[0];
        assert_eq!(
            (r.name.as_str(), r.owner.as_str()),
            ("o/we<U+202E>ird", "o<U+001B>[2J")
        );
        let m = gh::RepoMeta {
            description: "d\u{202e}x".into(),
            topics: vec!["t\u{200b}".into()],
            ..Default::default()
        };
        write_meta("o/r", &m, "octocat");
        let got = read_meta("o/r", 300).unwrap();
        assert_eq!(
            (got.description.as_str(), got.topics[0].as_str()),
            ("d<U+202E>x", "t<U+200B>")
        );
    }

    #[cfg(unix)]
    #[test]
    fn reactor_names_are_fetched_on_demand_with_one_call_per_comment() {
        let shim = crate::testshim::Shim::new();
        let mut a = live("");
        a.panels[0].items = vec![pr(7)];
        let mut card = gh::Card {
            id: "C1".into(),
            ..Default::default()
        };
        card.reactions = vec![gh::Reaction {
            content: "THUMBS_UP".into(),
            count: 2,
            users: vec![],
        }];
        let cd: gh::CommentsData = vec![gh::Entry {
            head: "h".into(),
            body: "b".into(),
            resolved: false,
            thread: None,
            card,
        }]
        .into();
        a.cache.insert(
            (pr(7).key(), Tab::Comments),
            Load::Done(Ok(Data::Comments(cd))),
        );
        a.dtab = 2; // Comments
        shim.set(
            "graphql.out",
            r#"{"data":{"node":{"reactionGroups":[{"content":"THUMBS_UP","users":{"totalCount":2,"nodes":[{"login":"hubot"},{"login":"monalisa"}]}}]},"rateLimit":{"cost":1,"remaining":4000,"resetAt":"2030-01-01T00:00:00Z","limit":5000}}}"#,
        );
        a.detail_focus = true;
        a.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        wait(&mut a, "names", |a| {
            matches!(a.cache.get(&(pr(7).key(), Tab::Comments)),
                Some(Load::Done(Ok(Data::Comments(cd)))) if !cd[0].card.reactions[0].users.is_empty())
        });
        assert_eq!(
            shim.calls()
                .iter()
                .filter(|c| c.contains("api graphql"))
                .count(),
            1
        );
        // closing and reopening does not ask again: the names are known
        a.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        a.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        quiesce();
        assert_eq!(
            shim.calls()
                .iter()
                .filter(|c| c.contains("api graphql"))
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_confirmed_write_does_not_wait_behind_reads_or_hung_calls() {
        use std::sync::atomic::{AtomicBool, AtomicUsize};
        let shim = crate::testshim::Shim::new();
        // every pool worker busy (and more waiting) with reads that hang until the test lets go
        let (done, release) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
        );
        for _ in 0..8 {
            let (d, r) = (done.clone(), release.clone());
            pool::global().submit(
                Prio::User,
                || false,
                move || {
                    while !r.load(Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    d.fetch_add(1, Relaxed);
                },
                || {},
            );
        }
        let mut a = live("");
        a.modal = Some(Modal::Confirm(Confirm::new(
            ["gh", "api", "-X", "PATCH", "notifications/threads/9"]
                .map(String::from)
                .to_vec(),
            None,
        )));
        a.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        wait(&mut a, "the write", |_| {
            shim.calls().iter().any(|c| c.contains("PATCH"))
        });
        // structural, not timed: the write has run while every read is still stuck
        assert_eq!(
            done.load(Relaxed),
            0,
            "the write did not wait for the reads"
        );
        release.store(true, Relaxed);
        while done.load(Relaxed) < 8 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        wait(&mut a, "the result", |a| !a.status.starts_with("running"));
        assert_eq!(
            shim.calls().iter().filter(|c| c.contains("PATCH")).count(),
            1,
            "exactly once"
        );
    }

    #[test]
    fn jobs_the_pool_loses_are_reported_not_forgotten() {
        let mut a = plain();
        // what the lost-job paths send: a list error, a status, a cleared "in flight" mark
        a.panels[2].loading = true;
        a.panels[2].seq = 5;
        a.tx.send(Msg::List(
            PK::Issues,
            0,
            5,
            Err("could not load; try again (r)".into()),
        ))
        .unwrap();
        a.react_pending.insert("C1".into());
        a.tx.send(Msg::Reactors(
            "k".into(),
            "C1".into(),
            a.dgen,
            Err("request lost".into()),
        ))
        .unwrap();
        a.poll();
        assert!(
            !a.panels[2].loading && a.panels[2].error.is_some(),
            "not stuck on loading"
        );
        assert!(a.react_pending.is_empty() && a.status.contains("could not load who reacted"));
    }

    #[cfg(unix)]
    #[test]
    fn a_long_repo_list_arrives_a_page_per_job_and_is_cached_once() {
        let shim = crate::testshim::Shim::new();
        let page = |n: &str, next: bool| {
            format!(
                r#"{{"data":{{"viewer":{{"login":"octocat","repositories":{{"pageInfo":{{"hasNextPage":{next},"endCursor":"c"}},"nodes":[{{"nameWithOwner":"o/{n}","isPrivate":false,"isFork":false,"isArchived":false,"stargazerCount":1,"pushedAt":"2026-01-01T00:00:00Z","primaryLanguage":null,"owner":{{"__typename":"User","login":"o"}},"issues":{{"totalCount":0}},"pullRequests":{{"totalCount":0}}}}]}}}}}}}}"#
            )
        };
        shim.set("graphql.1.out", &page("one", true));
        shim.set("graphql.2.out", &page("two", true));
        shim.set("graphql.3.out", &page("three", false));
        let mut a = live("");
        a.open_browser(true);
        wait(&mut a, "all pages", |a| {
            a.browser.as_ref().is_some_and(|b| !b.loading)
        });
        let names: Vec<_> = a
            .browser
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .map(|r| r.name.clone())
            .collect();
        assert_eq!(names, ["o/one", "o/two", "o/three"]);
        assert!(
            shim.calls()[1].contains("after=c"),
            "pages follow the cursor: {:?}",
            shim.calls()
        );
        let (c, _) = crate::cache::Store::default_if_enabled()
            .unwrap()
            .read::<CachedRepos>(&repos_key("github.com", "octocat"), rate::now())
            .unwrap();
        assert_eq!(
            c.rows.len(),
            3,
            "the whole list is cached, once, at the end"
        );
        // closing and reopening drops the old listing's remaining pages (its jobs are stale)
        a.open_browser(true);
        a.open_browser(true);
        assert_eq!(a.repos_a.load(Relaxed), a.repos_seq);
    }

    fn key(a: &mut App, c: char) {
        a.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }

    /// Overview requests (`pr view` with the overview fields; the headRefOid lookups belong to the diff).
    fn views(shim: &crate::testshim::Shim) -> usize {
        shim.calls()
            .iter()
            .filter(|c| c.contains("pr view") && c.contains("isDraft"))
            .count()
    }

    #[cfg(unix)]
    #[test]
    fn scrolling_through_a_list_fetches_only_the_item_you_rest_on() {
        let shim = crate::testshim::Shim::new();
        let mut a = live("");
        a.panels[0].items = (1..=12).map(pr).collect();
        a.ensure(); // a tick before scrolling starts
        // ten presses with the gap a real terminal user (or a slow redraw) leaves: after each
        // press the loop idles 100ms and runs a tick, then the next key arrives
        for _ in 0..10 {
            key(&mut a, 'j');
            a.last_input = std::time::Instant::now() - std::time::Duration::from_millis(120);
            a.ensure();
        }
        quiesce();
        assert_eq!(
            views(&shim),
            0,
            "nothing was fetched while the cursor kept moving: {:?}",
            shim.calls()
        );
        // the cursor rests: one tick registers the item, the next fetches it, and only it
        a.last_input = std::time::Instant::now() - std::time::Duration::from_millis(150);
        a.ensure();
        a.ensure();
        wait(&mut a, "the overview", |_| views(&shim) >= 1);
        quiesce();
        assert_eq!(views(&shim), 1, "{:?}", shim.calls());
        assert!(
            shim.calls()
                .iter()
                .any(|c| c.contains("pr view 11") && c.contains("isDraft")),
            "{:?}",
            shim.calls()
        );
    }

    #[cfg(unix)]
    #[test]
    fn comments_are_fetched_once_and_only_when_asked_for() {
        let shim = crate::testshim::Shim::new();
        let comments = |c: &crate::testshim::Shim| {
            c.calls()
                .iter()
                .filter(|x| x.contains("comments(first"))
                .count()
        };
        let empty = r#"{"data":{"repository":{"pullRequest":{"comments":{"totalCount":0,"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]},"reviews":{"totalCount":0,"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]},"reviewThreads":{"totalCount":0,"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}},"rateLimit":{"cost":1,"remaining":4000,"resetAt":"2030-01-01T00:00:00Z","limit":5000}}}"#;
        shim.set("graphql.out", empty);
        let mut a = live("");
        a.panels[0].items = vec![pr(1), pr(2)];
        // selecting a PR on its Overview, Checks and Diff tabs, however many ticks pass: no comments request
        for _ in 0..4 {
            a.ensure();
            quiesce();
        }
        key(&mut a, ']'); // Checks
        a.last_input = std::time::Instant::now() - std::time::Duration::from_secs(1);
        for _ in 0..3 {
            a.ensure();
            quiesce();
        }
        assert!(
            shim.calls()
                .iter()
                .filter(|c| c.contains("reviewThreads"))
                .all(|c| !c.contains("body") && !c.contains("reactionGroups")),
            "the Files badges ask for thread paths only: {:?}",
            shim.calls()
        );
        assert_eq!(comments(&shim), 0, "{:?}", shim.calls());
        // the Comments tab: one request, however often the tick repeats and the key is pressed
        while a.cur_tab() != Tab::Comments {
            key(&mut a, ']');
        }
        a.last_input = std::time::Instant::now() - std::time::Duration::from_secs(1);
        for _ in 0..6 {
            a.ensure();
            a.last_input = std::time::Instant::now() - std::time::Duration::from_secs(1);
        }
        wait(&mut a, "comments", |_| comments(&shim) >= 1);
        for _ in 0..6 {
            a.ensure();
            a.poll();
            quiesce();
        }
        assert_eq!(
            comments(&shim),
            1,
            "in-flight and finished jobs are not queued again: {:?}",
            shim.calls()
        );
        // the drill-in asks for it once too (plus the other three tabs)
        let before = comments(&shim);
        key(&mut a, '\u{0}');
        a.enter_ctx();
        for _ in 0..6 {
            a.last_input = std::time::Instant::now() - std::time::Duration::from_secs(1);
            a.ensure();
            a.poll();
            quiesce();
        }
        assert_eq!(comments(&shim), before, "already cached: not fetched again");
    }

    #[cfg(unix)]
    #[test]
    fn identical_requests_are_not_queued_twice() {
        let shim = crate::testshim::Shim::new();
        let mut a = live("");
        a.panels[0].items = vec![pr(3)];
        for _ in 0..5 {
            a.fetch(pr(3), Tab::Checks);
        }
        wait(&mut a, "checks", |_| {
            shim.calls().iter().any(|c| c.contains("pr checks"))
        });
        quiesce();
        a.fetch(pr(3), Tab::Checks);
        quiesce();
        let n = shim
            .calls()
            .iter()
            .filter(|c| c.contains("pr checks"))
            .count();
        assert_eq!(n, 1, "{:?}", shim.calls());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_list_or_count_is_unknown_not_zero_and_is_not_retried() {
        let shim = crate::testshim::Shim::new();
        let mut a = live("");
        // the list failed (timeout, error): the title says so, never "0"
        a.panels[0].error = Some("gh timed out after 60s".into());
        assert_eq!(a.active_count(0), None);
        assert!(a.tab_failed(0, 0) && !a.tab_failed(0, 1));
        a.panels[0].error = None;
        assert_eq!(
            a.active_count(0),
            Some((0, false)),
            "an empty successful list is a real 0"
        );
        // a count lookup that fails stays unknown, shows the cross, and is asked for once
        shim.set("prlist.err", "boom");
        a.ensure();
        wait(&mut a, "the failed count", |a| {
            a.count_failed.contains(&(PK::Prs, 2))
        });
        assert!(!a.counts.contains_key(&(PK::Prs, 2)) && a.tab_failed(0, 2));
        let asked = shim
            .calls()
            .iter()
            .filter(|c| c.contains("pr list"))
            .count();
        for _ in 0..4 {
            a.last_input = std::time::Instant::now() - std::time::Duration::from_secs(5);
            a.ensure();
            a.poll();
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        // (the Merged tab may be asked once; the failed All tab is not asked again)
        let all = |s: &crate::testshim::Shim| {
            s.calls()
                .iter()
                .filter(|c| c.contains("pr list") && !c.contains("--state=merged"))
                .count()
        };
        assert_eq!(
            all(&shim),
            1,
            "no retry loop: {asked} asked, {:?}",
            shim.calls()
        );
        // a reload forgets the failure
        a.reload(false);
        assert!(a.count_failed.is_empty());
    }

    fn gitem(repo: &str, n: u64) -> Item {
        Item {
            number: n,
            title: format!("{repo} item {n}"),
            repo: repo.into(),
            state: "open".into(),
            kind: Kind::Pr,
            url: format!("https://github.com/{repo}/pull/{n}"),
            ..Default::default()
        }
    }

    /// The global home, with `o/r` as the repo `G` leads to; nothing is fetched.
    fn home() -> App {
        let mut a = App::build_start(
            Some("o/r".into()),
            true,
            Theme::new(false, IconSet::Unicode, true),
            false,
            Config::default(),
        );
        a.panels[0].items = vec![gitem("a/b", 1), gitem("c/d", 2), gitem("e/f", 3)];
        a
    }

    fn press(a: &mut App, c: char) {
        a.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }

    #[test]
    fn the_global_home_has_its_own_panels_and_header() {
        let a = home();
        assert!(a.global);
        let kinds: Vec<_> = a.panels.iter().map(|p| p.kind).collect();
        assert_eq!(kinds, [PK::Review, PK::MyPrs, PK::Assigned, PK::Repos]);
        assert!(
            a.header.contains("all repos") && a.header.contains("scope: all"),
            "{}",
            a.header
        );
        assert!(
            !a.header.contains("o/r"),
            "no repo facts in the global header"
        );
        // sections are searches: never counted in the background
        assert!(a.panels.iter().take(3).all(|p| p.kind.is_global_search()));
        assert!(!a.count_wanted(1, 1) && !a.count_wanted(2, 2));
    }

    #[test]
    fn g_swaps_homes_and_each_remembers_its_cursor_tab_and_focus() {
        let mut a = home();
        a.panels[0].cursor = 2;
        press(&mut a, '2'); // My PRs
        press(&mut a, '}'); // its Merged tab
        a.panels[1].items = vec![gitem("a/b", 9)];
        assert_eq!((a.focus, a.panels[1].tab), (1, 1));
        press(&mut a, 'G');
        assert!(!a.global);
        assert_eq!(a.repo, "o/r");
        assert_eq!(a.panels[0].kind, PK::Prs);
        assert!(
            a.header.contains("o/r") && a.header.contains("branch"),
            "{}",
            a.header
        );
        a.panels[0].items = vec![gitem("o/r", 1), gitem("o/r", 2)];
        a.panels[0].cursor = 1;
        press(&mut a, '3'); // Issues
        press(&mut a, 'G');
        assert!(a.global);
        assert_eq!(
            (a.focus, a.panels[1].tab, a.panels[0].cursor),
            (1, 1, 2),
            "global as it was left"
        );
        assert_eq!(a.panels[1].items.len(), 1);
        press(&mut a, 'G');
        assert_eq!(
            (a.focus, a.panels[0].cursor),
            (2, 1),
            "and the repo as it was left"
        );
        assert_eq!(a.panels[0].items.len(), 2);
    }

    #[test]
    fn without_a_repo_there_is_nothing_for_g_to_lead_to() {
        let mut a = App::build_start(
            None,
            true,
            Theme::new(false, IconSet::Unicode, true),
            false,
            Config::default(),
        );
        press(&mut a, 'G');
        assert!(a.global && a.status.contains("no repo yet"), "{}", a.status);
        // but an item's repo can be opened, and then G leads back
        a.panels[0].items = vec![gitem("x/y", 4)];
        press(&mut a, 'S');
        assert!(!a.global && a.repo == "x/y" && a.from_global);
        assert!(a.header.contains("all repos \u{203a} x/y"), "{}", a.header);
        press(&mut a, 'G');
        assert!(a.global && a.panels[0].items.len() == 1);
    }

    #[test]
    fn s_in_a_repo_explains_and_s_in_the_home_opens_the_picker() {
        let mut a = App::with(
            "o/r".into(),
            Theme::new(false, IconSet::Unicode, true),
            false,
        );
        press(&mut a, 's');
        assert!(
            a.modal.is_none() && a.status.contains("global home"),
            "{}",
            a.status
        );
        let mut h = home();
        press(&mut h, 's');
        assert!(matches!(h.modal, Some(Modal::Scope(_))));
    }

    #[test]
    fn the_scope_picker_sets_persists_and_reloads_the_sections() {
        let dir = std::env::temp_dir().join(format!("gh-tui-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut a = home();
        a.state_dir = Some(dir.clone());
        a.cfg.repos.favorites = vec!["a/b".into()];
        let saved = || crate::state::load_scope(&dir.join("scope.json"), "github.com");
        // Favorites
        press(&mut a, 's');
        press(&mut a, 'j');
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(a.modal.is_none() && a.scope == Scope::Favorites);
        assert_eq!(saved().as_deref(), Some("favorites"));
        assert!(a.header.contains("scope: favorites"));
        assert!(
            a.panels[0].loading && a.panels[0].items.is_empty(),
            "the focused section reloads"
        );
        assert!(
            a.panels[1].unloaded && a.panels[2].unloaded,
            "the others when focused"
        );
        // Org: the list arrives from a message, typing filters it
        press(&mut a, 's');
        for _ in 0..2 {
            a.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        a.tx.send(Msg::Orgs(Ok(vec![
            "cli".into(),
            "acme".into(),
            "bad name".into(),
        ])))
        .unwrap();
        a.poll();
        let Some(Modal::Scope(p)) = &a.modal else {
            panic!("still picking")
        };
        assert_eq!(p.stage, ScopeStage::Orgs);
        press(&mut a, 'a');
        let Some(Modal::Scope(p)) = &a.modal else {
            panic!()
        };
        let names: Vec<_> = a.scope_choices(p).into_iter().map(|c| c.0).collect();
        assert_eq!(
            names,
            ["acme", "Use org 'a'"],
            "filtered by the typed text, plus the typed name as a free-text org"
        );
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(a.scope, Scope::Org("acme".into()));
        assert_eq!(saved().as_deref(), Some("org:acme"));
        // Repo: a typed owner/name is accepted; junk is not offered
        press(&mut a, 's');
        for _ in 0..3 {
            a.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for c in "x/y z".chars() {
            press(&mut a, c);
        }
        let Some(Modal::Scope(p)) = &a.modal else {
            panic!()
        };
        assert!(
            a.scope_choices(p)
                .iter()
                .all(|c| !matches!(&c.1, ScopeChoice::Set(Scope::Repo(r)) if r.contains(' ')))
        );
        a.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        a.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(a.scope, Scope::Repo("x/y".into()));
        assert_eq!(saved().as_deref(), Some("repo:x/y"));
        assert!(a.header.contains("scope: repo:x/y"));
        // Esc closes without changing anything
        press(&mut a, 's');
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(a.modal.is_none() && a.scope == Scope::Repo("x/y".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drilling_into_a_pr_keeps_the_global_context() {
        let mut a = home();
        a.panels[0].cursor = 1;
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(a.ctx.is_some() && a.global, "drill-in inside the home");
        assert_eq!(a.ctx.as_ref().unwrap().pr.repo, "c/d");
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(a.ctx.is_none() && a.global);
        assert_eq!(
            (a.focus, a.panels[0].cursor),
            (0, 1),
            "back at the same row"
        );
    }

    #[test]
    fn hidden_repos_are_counted_in_the_section_not_listed() {
        let mut a = home();
        a.panels[0].seq = 4;
        a.tx.send(Msg::GMeta(PK::Review, 0, 4, 3, 0, None)).unwrap();
        a.tx.send(Msg::GMeta(PK::Review, 0, 3, 99, 0, None))
            .unwrap(); // a stale answer
        a.poll();
        assert_eq!(a.panels[0].hidden, 3);
        a.tx.send(Msg::GMeta(PK::Review, 0, 4, 0, 5, None)).unwrap();
        a.poll();
        assert!(a.status.contains("+5 favorites not shown"), "{}", a.status);
    }

    #[cfg(unix)]
    #[test]
    fn detail_calls_for_a_row_of_the_home_carry_that_rows_repo() {
        let shim = crate::testshim::Shim::new();
        let mut a = home();
        a.net = true;
        a.panels[0].items = vec![gitem("other/repo", 7)];
        a.last_input = std::time::Instant::now() - std::time::Duration::from_secs(60);
        a.ensure();
        a.ensure(); // the Overview waits a tick
        wait(&mut a, "the calls", |_| {
            let c = shim.calls();
            c.iter()
                .any(|x| x.starts_with("pr view 7 -R other/repo --json isDraft"))
                && c.iter()
                    .any(|x| x.contains("pr view 7 -R other/repo --json headRefOid"))
        });
        assert!(
            shim.calls()
                .iter()
                .filter(|c| c.starts_with("pr "))
                .all(|c| c.contains("-R other/repo")),
            "{:?}",
            shim.calls()
        );
        // and the actions carry it too
        let acts = crate::act::actions(a.selected(), 0, "", &a.sel());
        let approve = acts
            .iter()
            .find(|x| x.label.starts_with("Approve"))
            .unwrap();
        assert_eq!(
            &(approve.build)("")[..],
            ["gh", "pr", "review", "7", "-R", "other/repo", "--approve"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_global_start_searches_only_the_focused_section_and_the_rest_on_focus() {
        let shim = crate::testshim::Shim::new();
        shim.set("searchprs.out", "[]");
        shim.set("searchissues.out", "[]");
        let mut a = App::build_start(
            None,
            true,
            Theme::new(false, IconSet::Unicode, true),
            true,
            Config::default(),
        );
        wait(&mut a, "the first section", |a| !a.panels[0].loading);
        let searches = |s: &crate::testshim::Shim| {
            s.calls()
                .into_iter()
                .filter(|c| c.starts_with("search "))
                .collect::<Vec<_>>()
        };
        assert_eq!(searches(&shim).len(), 1, "{:?}", shim.calls());
        assert!(searches(&shim)[0].contains("review-requested:@me"));
        assert!(
            a.user == "octocat" && a.header.contains("user: octocat"),
            "{}",
            a.header
        );
        assert!(
            !shim
                .calls()
                .iter()
                .any(|c| c.starts_with("repo view") || c.starts_with("api user")),
            "no repo lookups: {:?}",
            shim.calls()
        );
        press(&mut a, '3'); // Issues -> a search on first focus
        wait(&mut a, "the third section", |a| !a.panels[2].loading);
        assert_eq!(searches(&shim).len(), 2);
        assert!(
            searches(&shim)[1].starts_with("search issues")
                && searches(&shim)[1].contains("assignee:@me")
        );
        // no background counting or polling of searches, however long it idles
        for _ in 0..5 {
            a.last_input = std::time::Instant::now() - std::time::Duration::from_secs(10);
            a.ensure();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(searches(&shim).len(), 2);
        // a changed scope reloads the focused section only
        a.set_scope(Scope::Org("cli".into()));
        wait(&mut a, "reload", |a| !a.panels[2].loading);
        assert_eq!(searches(&shim).len(), 3);
        assert!(searches(&shim)[2].ends_with("org:cli"));
    }

    #[test]
    fn the_files_panel_follows_the_pr_list_you_were_last_in() {
        let cfg = config::parse(
            "[panels]\nglobal = [\"review\", \"mine\", \"files\"]\n",
            "t",
        )
        .unwrap();
        let mut a = App::build_start(
            None,
            true,
            Theme::new(false, IconSet::Unicode, true),
            false,
            cfg,
        );
        a.panels[0].items = vec![gitem("a/b", 1)];
        a.panels[1].items = vec![gitem("c/d", 2), gitem("c/d", 3)];
        assert_eq!(
            a.pr_item().map(|i| i.number),
            Some(1),
            "the first PR list until you go elsewhere"
        );
        press(&mut a, '2');
        a.panels[1].cursor = 1;
        press(&mut a, '3'); // Files
        assert_eq!(a.panels[a.focus].kind, PK::Files);
        assert_eq!(
            a.pr_item().map(|i| (i.repo.as_str(), i.number)),
            Some(("c/d", 3))
        );
        // an issue row is not a PR: nothing for Files to show
        let cfg = config::parse(
            "[panels]\nglobal = [\"assigned\", \"review\", \"files\"]\n",
            "t",
        )
        .unwrap();
        let mut b = App::build_start(
            None,
            true,
            Theme::new(false, IconSet::Unicode, true),
            false,
            cfg,
        );
        b.panels[0].items = vec![Item {
            kind: Kind::Issue,
            ..gitem("a/b", 5)
        }];
        assert!(b.pr_item().is_none());
    }

    fn repo_row(name: &str, private: bool, lang: &str) -> RepoRow {
        RepoRow {
            name: name.into(),
            owner: name.split('/').next().unwrap().into(),
            private,
            lang: lang.into(),
            stars: 3,
            pushed: "2026-01-01T00:00:00Z".into(),
            ..Default::default()
        }
    }

    /// The home with a config file to save to, favorites and recent repos, and a state directory.
    fn repos_home(tag: &str) -> (App, PathBuf) {
        let dir = std::env::temp_dir().join(format!("gh-tui-repos-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut a = home();
        a.cfg_path = Some(dir.join("config.toml"));
        a.state_dir = Some(dir.clone());
        a.cfg.repos.favorites = vec!["a/b".into(), "c/d".into()];
        a.recent.repos = vec!["e/f".into(), "a/b".into(), "g/h".into()];
        a.refresh_repos_panels();
        (a, dir)
    }

    #[cfg(unix)]
    #[test]
    fn the_repos_panel_lists_favorites_and_recent_from_local_data_only() {
        let shim = crate::testshim::Shim::new();
        let store = crate::cache::Store::default_if_enabled().unwrap();
        let c = CachedRepos {
            host: "github.com".into(),
            viewer: "octocat".into(),
            rows: vec![repo_row("a/b", true, "Go")],
            truncated: false,
        };
        store
            .write(&repos_key("github.com", "octocat"), &c, rate::now())
            .unwrap();
        let (mut a, dir) = repos_home("local");
        assert_eq!(a.panels[3].kind, PK::Repos);
        let names = |a: &App| {
            a.panels[3]
                .items
                .iter()
                .map(|i| i.repo.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&a), ["a/b", "c/d"]);
        assert_eq!(a.panels[3].items[0].state, "fav");
        assert!(
            a.panels[3].items[0].meta.starts_with("private \u{b7} Go"),
            "{}",
            a.panels[3].items[0].meta
        );
        assert!(
            a.panels[3].items[1].meta.is_empty(),
            "nothing is looked up for repos the cache doesn't know"
        );
        press(&mut a, '4');
        press(&mut a, '}');
        assert_eq!(names(&a), ["e/f", "a/b", "g/h"], "Recent, newest first");
        assert_eq!(
            a.panels[3].items[1].state, "fav",
            "a favorite is starred in Recent too"
        );
        // both counts are known at once (title: Fav 2 / Rec 3)
        assert_eq!(
            (a.tab_count(3, 0), a.tab_count(3, 1)),
            (Some((2, false)), Some((3, false)))
        );
        assert!(
            shim.calls().is_empty(),
            "no gh process for any of it: {:?}",
            shim.calls()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enter_on_a_repo_opens_it_and_records_it_as_recent() {
        let (mut a, dir) = repos_home("enter");
        press(&mut a, '4');
        a.panels[3].cursor = 1; // c/d
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!a.global && a.repo == "c/d" && a.from_global);
        assert_eq!(a.recent.repos[0], "c/d");
        let text = std::fs::read_to_string(dir.join("recent.json")).unwrap();
        assert!(
            text.contains("\"c/d\"")
                && text.contains("github.com")
                && !text.to_lowercase().contains("token"),
            "{text}"
        );
        // S does the same from any row of the home
        press(&mut a, 'G');
        press(&mut a, '1');
        press(&mut a, 'S');
        assert_eq!(a.repo, "a/b", "the selected PR's repo");
        assert_eq!(a.recent.repos[..2], ["a/b".to_string(), "c/d".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn f_toggles_a_favorite_h_hides_and_s_scopes_the_home_to_a_repo() {
        let (mut a, dir) = repos_home("keys");
        press(&mut a, '4');
        // f: the same favorites list the browser edits, saved to the config file
        press(&mut a, 'f');
        assert_eq!(a.cfg.repos.favorites, ["c/d"], "a/b was a favorite");
        assert!(
            std::fs::read_to_string(dir.join("config.toml"))
                .unwrap()
                .contains("c/d")
        );
        assert_eq!(a.panels[3].items.len(), 1);
        assert!(
            a.status.contains("removed a/b from favorites"),
            "{}",
            a.status
        );
        a.panels[3].tab = 1;
        a.refresh_repos_panels();
        a.panels[3].cursor = 0; // e/f
        press(&mut a, 'f');
        assert!(a.cfg.repos.is_fav("e/f"), "adding from Recent");
        // H: hidden everywhere; loaded sections just lose its rows, no search is spent
        a.panels[0].unloaded = false;
        a.panels[0].items = vec![gitem("e/f", 1), gitem("a/b", 2), gitem("e/f", 3)];
        a.panels[1].items = vec![gitem("e/f", 4)];
        press(&mut a, 'H');
        assert!(a.cfg.repos.is_hidden("e/f"));
        assert!(!a.panels[0].unloaded && !a.panels[0].loading, "no reload");
        assert_eq!(
            a.panels[0]
                .items
                .iter()
                .map(|i| i.number)
                .collect::<Vec<_>>(),
            [2]
        );
        assert_eq!(
            (
                a.panels[0].hidden,
                a.panels[1].items.len(),
                a.panels[1].hidden
            ),
            (2, 0, 1)
        );
        // unhiding cannot bring the rows back by itself: the sections are searched again
        press(&mut a, 'H');
        assert!(!a.cfg.repos.is_hidden("e/f") && a.panels[0].unloaded);
        // s: scope the home to that repo
        a.panels[3].cursor = 1; // a/b
        press(&mut a, 's');
        assert!(
            a.global && a.scope == Scope::Repo("a/b".into()),
            "{:?}",
            a.scope
        );
        assert!(a.header.contains("scope: repo:a/b"));
        assert_eq!(
            crate::state::load_scope(&dir.join("scope.json"), "github.com").as_deref(),
            Some("repo:a/b")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_refused_config_save_leaves_favorites_alone() {
        let (mut a, dir) = repos_home("refused");
        std::fs::write(dir.join("config.toml"), "ascii = [broken\n").unwrap();
        press(&mut a, '4');
        press(&mut a, 'f');
        assert_eq!(
            a.cfg.repos.favorites,
            ["a/b", "c/d"],
            "memory and file agree"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_repos_panel_can_be_added_to_the_repo_layout_but_is_not_there_by_default() {
        let a = App::with(
            "o/r".into(),
            Theme::new(false, IconSet::Unicode, true),
            false,
        );
        assert!(a.panels.iter().all(|p| p.kind != PK::Repos));
        let cfg = config::parse("[panels]\nshow = [\"prs\", \"repos\"]\n", "t").unwrap();
        let mut b = App::build(
            "o/r".into(),
            Theme::new(false, IconSet::Unicode, true),
            false,
            cfg,
        );
        b.cfg.repos.favorites = vec!["x/y".into()];
        b.refresh_repos_panels();
        assert_eq!(b.panels[1].kind, PK::Repos);
        press(&mut b, '2');
        b.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            b.repo, "x/y",
            "Enter switches the repo, right where you are"
        );
        assert!(!b.global && !b.from_global);
    }

    #[test]
    fn an_inbox_item_opens_its_repo_from_the_global_home() {
        let mut a = home();
        let mut n = gitem("x/y", 9);
        n.cmd = "77".into();
        a.seed_inbox(vec![n]);
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(a.inbox.is_none() && !a.global && a.repo == "x/y" && a.from_global);
        assert_eq!(a.pending_select, Some((PK::Prs, 9, "x/y".into())));
    }

    #[test]
    fn r_reloads_the_focused_global_section_and_the_repos_panel() {
        let mut a = home();
        a.panels[0].seq = 3;
        press(&mut a, 'r');
        assert!(
            a.panels[0].loading && a.panels[0].seq != 3,
            "a fresh, never-reused number"
        );
        press(&mut a, '4');
        a.cfg.repos.favorites = vec!["a/b".into()];
        press(&mut a, 'r');
        assert_eq!(a.panels[3].items.len(), 1, "the local list is rebuilt");
    }

    /// `gh search prs --json` output for these (repo, number) pairs.
    fn gsearch(rows: &[(&str, u64)]) -> String {
        let rows: Vec<String> = rows
            .iter()
            .map(|(r, n)| {
                format!(
                    r#"{{"number":{n},"title":"t{n}","url":"https://github.com/{r}/pull/{n}","state":"open","isDraft":false,"author":{{"login":"a"}},"labels":[],"body":"","repository":{{"nameWithOwner":"{r}"}},"updatedAt":"2026-01-{:02}T00:00:00Z"}}"#,
                    n % 28 + 1
                )
            })
            .collect();
        format!("[{}]", rows.join(","))
    }

    fn list_reply(a: &mut App, kind: PK, tab: usize, seq: u64, items: Vec<Item>) {
        a.tx.send(Msg::List(kind, tab, seq, Ok(items))).unwrap();
        a.poll();
    }

    #[test]
    fn a_late_reply_from_the_repo_we_left_never_lands_in_the_repo_we_opened() {
        let mut a = home();
        press(&mut a, 'G'); // repo A = o/r
        let (tab, seq_a) = (a.panels[0].tab, a.panels[0].seq);
        press(&mut a, 'G');
        a.panels[0].items = vec![gitem("x/y", 9)];
        press(&mut a, 'S'); // repo B = x/y, a brand new side (A's was replaced)
        assert_eq!(a.repo, "x/y");
        let seq_b = a.panels[0].seq;
        assert_ne!(seq_a, seq_b, "numbers are never reused across homes");
        assert!(a.panels[0].loading && a.panels[0].items.is_empty());
        // A's reply arrives before B's own: dropped, B still waiting
        list_reply(&mut a, PK::Prs, tab, seq_a, vec![gitem("o/r", 1)]);
        assert!(
            a.panels[0].items.is_empty() && a.panels[0].loading,
            "A's list did not land in B"
        );
        // B's reply lands; A's duplicate afterwards changes nothing
        list_reply(&mut a, PK::Prs, tab, seq_b, vec![gitem("x/y", 2)]);
        assert_eq!(a.panels[0].items[0].repo, "x/y");
        assert!(!a.panels[0].loading);
        list_reply(
            &mut a,
            PK::Prs,
            tab,
            seq_a,
            vec![gitem("o/r", 3), gitem("o/r", 4)],
        );
        assert_eq!(a.panels[0].items.len(), 1);
        assert_eq!(a.selected().unwrap().repo, "x/y", "actions would target B");
        // facts and branch for A are dropped too (they are keyed by repo)
        a.tx.send(Msg::Meta(
            "o/r".into(),
            gh::RepoMeta {
                stars: 99,
                ..Default::default()
            },
        ))
        .unwrap();
        a.tx.send(Msg::Branch("o/r".into(), Some("feat".into())))
            .unwrap();
        a.tx.send(Msg::StartupFallback("o/r".into())).unwrap();
        a.poll();
        assert!(a.meta.is_none() && a.cwd_branch.is_none() && !a.header.contains("feat"));
    }

    #[test]
    fn replies_for_a_parked_home_reach_their_own_panels_and_counts() {
        let mut a = home();
        press(&mut a, 'G'); // repo o/r on screen, global parked
        let g_seq = a.other.as_ref().unwrap().panels[0].seq;
        let r_seq = a.panels[0].seq;
        press(&mut a, 'G'); // global on screen, repo parked
        let (gtab, rtab) = (a.panels[0].tab, 0);
        a.panels[0].loading = true;
        a.panels[0].seq = next_seq();
        let live_seq = a.panels[0].seq;
        let counts_before = a.counts.clone();
        // the repo's list arrives while the global home is showing
        list_reply(
            &mut a,
            PK::Prs,
            rtab,
            r_seq,
            vec![gitem("o/r", 1), gitem("o/r", 2)],
        );
        let o = a.other.as_ref().unwrap();
        assert_eq!(o.panels[0].items.len(), 2, "into the parked repo panel");
        assert_eq!(
            o.counts.get(&(PK::Prs, 0)),
            Some(&(2, false)),
            "its count in its own map"
        );
        assert_eq!(
            a.counts, counts_before,
            "the active home's counts are untouched"
        );
        assert!(
            a.panels[0].items.len() == 3 && a.panels[0].loading,
            "and nothing in the global list"
        );
        // an old global reply (before a scope change) is ignored; the fresh one lands
        let _ = g_seq;
        list_reply(
            &mut a,
            PK::Review,
            gtab,
            live_seq - 1,
            vec![gitem("z/z", 7)],
        );
        assert_eq!(a.panels[0].items.len(), 3);
        list_reply(&mut a, PK::Review, gtab, live_seq, vec![gitem("z/z", 8)]);
        assert_eq!(a.panels[0].items[0].number, 8);
    }

    #[test]
    fn toggling_changing_scope_and_opening_a_repo_while_loads_are_in_flight() {
        let mut a = home();
        let tab = a.panels[0].tab;
        a.panels[0].loading = true;
        a.panels[0].seq = next_seq();
        let old = a.panels[0].seq;
        a.set_scope(Scope::Org("cli".into())); // reloads the focused section: a new number
        let fresh = a.panels[0].seq;
        assert_ne!(old, fresh);
        a.panels[0].items = vec![gitem("cli/cli", 5)];
        press(&mut a, 'S'); // opens the row's repo; the global home is parked
        assert!(!a.global);
        list_reply(&mut a, PK::Review, tab, old, vec![gitem("old/scope", 1)]);
        list_reply(&mut a, PK::Review, tab, fresh, vec![gitem("cli/cli", 2)]);
        press(&mut a, 'G');
        let items: Vec<_> = a.panels[0].items.iter().map(|i| i.repo.clone()).collect();
        assert_eq!(
            items,
            ["cli/cli"],
            "only the reply for the scope in force survived"
        );
    }

    #[test]
    fn the_scope_picker_reads_the_repo_list_once_not_per_key_or_frame() {
        let mut a = home();
        let reads = || READS.with(|r| r.get());
        press(&mut a, 's');
        for _ in 0..3 {
            a.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        let base = reads();
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // Repo... stage
        assert_eq!(reads(), base + 1, "read once on entering the stage");
        for c in "abc/d".chars() {
            press(&mut a, c);
            let Some(Modal::Scope(p)) = &a.modal else {
                panic!()
            };
            let _ = a.scope_choices(p);
            let _ = a.scope_choices(p);
        }
        a.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(
            reads(),
            base + 1,
            "typing and re-rendering never touch the disk"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_section_refresh_is_bounded_and_respects_the_search_budget() {
        let shim = crate::testshim::Shim::new();
        shim.set("searchprs.out", "[]");
        shim.set("searchissues.out", "[]");
        let mut a = home();
        a.net = true;
        a.cfg.repos.favorites = (0..16).map(|i| format!("o/r{i}")).collect();
        a.scope = Scope::Favorites;
        a.panels[0].kind = PK::Involved; // PRs + issues per chunk: the worst case, 4 chunks x 2
        wait(&mut a, "start", |_| true);
        a.load_panel(0, false);
        wait(&mut a, "the section", |a| !a.panels[0].loading);
        let n = shim
            .calls()
            .iter()
            .filter(|c| c.starts_with("search "))
            .count();
        assert_eq!(
            n, 8,
            "never more than the 8 searches of a full favorites scope"
        );
        // with too little search quota left the refresh is refused, with the reset time
        let calls = shim.calls().len();
        a.rate.search = Some(rate::Bucket {
            limit: 30,
            remaining: 3,
            reset: rate::now() + 600,
        });
        a.panels[0].items = vec![gitem("a/b", 1)];
        a.load_panel(0, true);
        assert!(
            !a.panels[0].loading && a.panels[0].items.len() == 1,
            "the list is kept"
        );
        assert!(
            a.status.starts_with("search quota low, resets "),
            "{}",
            a.status
        );
        assert_eq!(shim.calls().len(), calls, "no search was made");
        // an empty list shows the reason instead of a blank
        a.panels[0].items.clear();
        a.load_panel(0, true);
        assert!(
            a.panels[0]
                .error
                .as_deref()
                .unwrap()
                .starts_with("search quota low")
        );
        // and a window that has reset is not held against us
        a.rate.search = Some(rate::Bucket {
            limit: 30,
            remaining: 0,
            reset: rate::now() - 1,
        });
        a.load_panel(0, true);
        assert!(a.panels[0].loading);
        wait(&mut a, "the section", |a| !a.panels[0].loading);
    }

    fn section(title: &str, kind: config::SectionKind, filter: &str) -> config::SectionCfg {
        config::SectionCfg {
            title: title.into(),
            kind,
            filter: filter.into(),
            limit: None,
            at: config::Where::Global,
        }
    }

    /// The global home with two custom sections (panels 5 and 6); nothing is fetched yet.
    fn sec_home() -> App {
        let cfg = Config {
            sections: vec![
                section(
                    "Review (acme)",
                    config::SectionKind::Prs,
                    "is:open review-requested:@me",
                ),
                section(
                    "Mine \u{202e}evil",
                    config::SectionKind::Issues,
                    "is:open org:own author:@me",
                ),
            ],
            ..Default::default()
        };
        let mut a = App::build_start(
            Some("o/r".into()),
            true,
            Theme::new(false, IconSet::Unicode, true),
            false,
            cfg,
        );
        a.panels.iter_mut().for_each(|p| p.unloaded = true);
        a
    }

    #[test]
    fn custom_sections_are_extra_panels_that_know_their_kind_and_scope() {
        let a = sec_home();
        let kinds: Vec<_> = a.panels.iter().skip(4).map(|p| p.kind).collect();
        assert_eq!(kinds, [PK::CustomPr(0), PK::CustomIssue(1)]);
        assert!(
            kinds.iter().all(|k| k.is_global_search())
                && kinds[0].is_pr_list()
                && !kinds[1].is_pr_list()
        );
        assert_eq!(a.panels[4].title, "Review (acme)");
        assert!(
            a.panels[5].title.ends_with("(own filter)") && !a.panels[5].title.contains('\u{202e}'),
            "{:?}: marked, and the bidi override is made visible",
            a.panels[5].title
        );
        assert!(a.scope_applies_to(4) && !a.scope_applies_to(5) && a.scope_applies_to(0));
        assert_eq!(
            a.active_count(4),
            None,
            "no count until it has been searched"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_custom_section_loads_only_when_focused_with_one_search_per_refresh() {
        let shim = crate::testshim::Shim::new();
        shim.set(
            "searchprs.out",
            &gsearch(&[("a/b", 1), ("noisy/bot", 2), ("c/d", 3)]),
        );
        let mut a = sec_home();
        a.net = true;
        a.cfg.repos.hidden = vec!["noisy/bot".into()];
        wait(&mut a, "start", |_| true);
        quiesce();
        assert!(
            shim.calls().iter().all(|c| !c.starts_with("search ")),
            "nothing searched before focus"
        );
        press(&mut a, '5');
        assert!(a.panels[4].loading);
        wait(&mut a, "the section", |a| !a.panels[4].loading);
        let searches = || {
            shim.calls()
                .into_iter()
                .filter(|c| c.starts_with("search "))
                .collect::<Vec<_>>()
        };
        assert_eq!(searches().len(), 1, "{:?}", searches());
        assert!(
            searches()[0].starts_with("search prs --limit=30 ")
                && searches()[0].ends_with("-- is:open review-requested:@me archived:false"),
            "{:?}",
            searches()
        );
        let repos: Vec<_> = a.panels[4].items.iter().map(|i| i.repo.as_str()).collect();
        assert_eq!(
            (repos, a.panels[4].hidden),
            (vec!["c/d", "a/b"], 1),
            "hidden repos are dropped"
        );
        assert_eq!(a.active_count(4), Some((2, false)));
        assert!(a.panels[5].unloaded, "the other section still waits");
        // the second section has a scope of its own: the `s` scope never reaches it
        press(&mut a, '6');
        wait(&mut a, "the section", |a| !a.panels[5].loading);
        assert!(
            searches()[1].starts_with("search issues ")
                && searches()[1].ends_with("-- is:open org:own author:@me archived:false")
        );
        a.set_scope(Scope::Org("acme".into()));
        assert!(
            !a.panels[5].unloaded,
            "own-scope section is not reloaded by a scope change"
        );
        quiesce();
        assert_eq!(
            searches().len(),
            2,
            "set_scope on the focused own-scope section does not search"
        );
        // r refreshes the focused one with exactly one search; the scope applies to the other
        press(&mut a, '5');
        wait(&mut a, "reload", |a| !a.panels[4].loading);
        press(&mut a, 'r');
        wait(&mut a, "refresh", |a| !a.panels[4].loading);
        let s = searches();
        assert_eq!(s.len(), 4, "{s:?}");
        assert!(
            s[2].ends_with("archived:false org:acme") && s[3].ends_with("archived:false org:acme"),
            "{s:?}"
        );
        // the quota guard refuses a refresh the minute cannot pay for and keeps the list
        a.rate.search = Some(rate::Bucket {
            limit: 30,
            remaining: 0,
            reset: rate::now() + 600,
        });
        press(&mut a, 'r');
        assert!(
            a.status.starts_with("search quota low, resets "),
            "{}",
            a.status
        );
        assert!(!a.panels[4].loading && a.panels[4].items.len() == 2);
        quiesce();
        assert_eq!(searches().len(), 4, "no search was made");
    }

    #[test]
    fn the_org_picker_accepts_an_org_you_are_not_a_member_of() {
        let dir = std::env::temp_dir().join(format!("gh-tui-freeorg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut a = home();
        a.state_dir = Some(dir.clone());
        a.orgs = Some(vec!["cli".into(), "acme".into()]);
        press(&mut a, 's');
        for _ in 0..2 {
            a.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let names = |a: &App| match &a.modal {
            Some(Modal::Scope(p)) => a
                .scope_choices(p)
                .into_iter()
                .map(|c| c.0)
                .collect::<Vec<_>>(),
            _ => panic!("picker closed"),
        };
        assert_eq!(names(&a), ["cli", "acme"]);
        for c in "charmbracelet".chars() {
            press(&mut a, c);
        }
        assert_eq!(
            names(&a),
            ["Use org 'charmbracelet'"],
            "no member matches, still selectable"
        );
        // a member match comes first so Enter on a partial name still picks the member org
        a.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        for _ in 0..12 {
            a.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        press(&mut a, 'a');
        assert_eq!(names(&a), ["acme", "Use org 'a'"]);
        // a name that is already a member org is not offered twice; junk is never offered
        press(&mut a, 'c');
        press(&mut a, 'm');
        press(&mut a, 'e');
        assert_eq!(names(&a), ["acme"]);
        for _ in 0..4 {
            a.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        for c in "bad name".chars() {
            press(&mut a, c);
        }
        assert!(names(&a).is_empty());
        for _ in 0..8 {
            a.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        for c in "charmbracelet".chars() {
            press(&mut a, c);
        }
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(a.modal.is_none());
        assert_eq!(a.scope, Scope::Org("charmbracelet".into()));
        assert_eq!(
            crate::state::load_scope(&dir.join("scope.json"), "github.com").as_deref(),
            Some("org:charmbracelet"),
            "persisted like any other scope"
        );
    }

    #[test]
    fn unhiding_a_repo_marks_repo_home_sections_stale_too() {
        let mut sc = section("Here", config::SectionKind::Prs, "is:open");
        sc.at = config::Where::Both;
        let cfg = Config {
            sections: vec![sc],
            ..Default::default()
        };
        let mut a = App::build(
            "o/r".into(),
            Theme::new(false, IconSet::Unicode, true),
            false,
            cfg,
        );
        let i = a.panels.len() - 1;
        assert!(a.panels[i].kind.custom().is_some());
        (a.panels[i].unloaded, a.panels[i].items) = (false, vec![gitem("o/r", 1)]);
        a.hidden_changed("x/y", false);
        assert!(a.panels[i].unloaded && a.panels[i].items.is_empty());
    }

    #[test]
    fn repo_sections_forget_the_old_repos_rows_and_count_on_a_repo_switch() {
        let mut sc = section("R", config::SectionKind::Prs, "is:open");
        sc.at = config::Where::Repo;
        let cfg = Config {
            sections: vec![sc],
            ..Default::default()
        };
        let mut a = App::build(
            "o1/alpha".into(),
            Theme::new(false, IconSet::Unicode, true),
            false,
            cfg,
        );
        let i = a.panels.len() - 1;
        (a.panels[i].unloaded, a.panels[i].items) =
            (false, vec![gitem("o1/alpha", 1), gitem("o1/alpha", 2)]);
        assert_eq!(a.active_count(i), Some((2, false)));
        a.switch_repo("o2/beta".into());
        assert!(a.panels[i].items.is_empty() && a.panels[i].unloaded);
        assert_eq!(
            a.active_count(i),
            None,
            "the title shows ? until it is focused and searched"
        );
    }

    #[test]
    fn r_on_a_section_that_is_already_searching_does_nothing() {
        let mut a = home();
        a.panels[0].loading = true;
        let seq = a.panels[0].seq;
        press(&mut a, 'r');
        assert_eq!(a.panels[0].seq, seq, "no second search");
        assert_eq!(a.status, "already searching");
        a.panels[0].loading = false;
        press(&mut a, 'r');
        assert_ne!(a.panels[0].seq, seq);
    }

    #[test]
    fn a_failed_search_note_reaches_the_status_line() {
        let mut a = home();
        let (tab, seq) = (a.panels[0].tab, a.panels[0].seq);
        a.tx.send(Msg::GMeta(
            PK::Review,
            tab,
            seq,
            0,
            0,
            Some("1 of 4 searches failed (boom); showing the rest".into()),
        ))
        .unwrap();
        a.poll();
        assert!(a.status.contains("1 of 4 searches failed"), "{}", a.status);
    }

    /// A repo browser with one hidden repo under the cursor, ready for `H`.
    fn browser_on(a: &mut App, name: &str) {
        let mut b = Browser::new();
        b.rows = vec![RepoRow {
            name: name.into(),
            owner: "e".into(),
            ..Default::default()
        }];
        (b.typing, b.loading, b.show_hidden) = (false, false, true);
        a.browser = Some(b);
    }

    fn mk(repo: &str, n: u64, who: &str, updated: &str) -> Item {
        let mut i = gitem(repo, n);
        (i.author.login, i.updated) = (who.into(), updated.into());
        i
    }

    #[test]
    fn g_groups_a_global_list_and_the_longest_waiting_group_comes_first() {
        let mut a = home();
        a.panels[0].items = vec![
            mk("a/b", 1, "bob", "2026-10-02T10:00:00Z"),
            mk("a/b", 2, "Alice", "2026-10-03T10:00:00Z"),
            mk("c/d", 3, "bob", "2026-09-30T10:00:00Z"),
            mk("c/d", 4, "alice", "2026-10-01T10:00:00Z"),
        ];
        (a.panels[0].unloaded, a.focus) = (false, 0);
        let order = |a: &App| a.visible(0).iter().map(|i| i.number).collect::<Vec<_>>();
        assert_eq!(order(&a), [1, 2, 3, 4], "flat: the search's order");
        press(&mut a, 'g');
        // bob has waited since 09-30, alice (any case) since 10-01
        assert_eq!((a.group, order(&a)), (GroupBy::Author, vec![1, 3, 2, 4]));
        assert!(a.header.contains("group: author"), "{}", a.header);
        press(&mut a, 'g');
        // c/d's oldest is 09-30, a/b's 10-02
        assert_eq!((a.group, order(&a)), (GroupBy::Repo, vec![3, 4, 1, 2]));
        press(&mut a, 'g');
        assert_eq!((a.group, order(&a)), (GroupBy::None, vec![1, 2, 3, 4]));
        assert!(!a.header.contains("group:"));
        // g still jumps to the top where it is not a global list
        a.focus = 0;
        a.detail_focus = true;
        press(&mut a, 'g');
        assert_eq!(a.group, GroupBy::None);
    }

    #[test]
    fn w_cycles_the_window_and_searches_pull_request_sections_again() {
        let _shim = crate::testshim::Shim::new();
        let mut a = home();
        a.focus = 0; // Review requested
        a.panels[0].items = vec![gitem("a/b", 1)];
        a.panels[0].unloaded = false;
        a.panels[2].unloaded = false; // Issues: not a pull request search
        a.panels[2].items = vec![gitem("a/b", 9)];
        assert_eq!(a.window, config::Window::Week, "7d by default");
        press(&mut a, 'W');
        assert_eq!(a.window, config::Window::Month);
        assert!(
            a.panels[0].loading && a.panels[0].items.is_empty(),
            "Review searches again"
        );
        assert_eq!(a.panels[2].items.len(), 1, "issues keep their rows");
        assert!(a.header.contains("PRs: 30d"), "{}", a.header);
        for _ in 0..3 {
            press(&mut a, 'W');
        }
        assert_eq!(a.window, config::Window::Week, "all, 24h, 7d");
    }

    #[test]
    fn h_on_a_review_row_hides_that_repo_without_a_search() {
        let dir = std::env::temp_dir().join(format!("gh-tui-hide-row-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut a = home();
        a.cfg_path = Some(dir.join("config.toml"));
        a.panels[0].items = vec![gitem("e/f", 5), gitem("a/b", 1), gitem("e/f", 6)];
        a.panels[0].unloaded = false;
        a.focus = 0;
        press(&mut a, 'H');
        assert!(a.cfg.repos.is_hidden("e/f"));
        assert_eq!(a.panels[0].items.len(), 1);
        assert_eq!((a.panels[0].hidden, a.panels[0].loading), (2, false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unhiding_in_the_browser_refreshes_the_global_sections_and_hiding_trims_them() {
        let dir = std::env::temp_dir().join(format!("gh-tui-unhide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut a = home();
        a.cfg_path = Some(dir.join("config.toml"));
        a.cfg.repos.hidden = vec!["e/f".into()];
        a.panels[0].items = vec![gitem("a/b", 1)];
        (a.panels[0].hidden, a.panels[1].unloaded) = (1, false);
        a.panels[1].items = vec![gitem("a/b", 2)];
        browser_on(&mut a, "e/f");
        press(&mut a, 'H'); // unhide
        assert!(!a.cfg.repos.is_hidden("e/f"));
        assert!(
            a.panels[0].loading && a.panels[0].items.is_empty(),
            "the focused section searches again"
        );
        assert_eq!(a.panels[0].hidden, 0, "the stale \"1 hidden\" is gone");
        assert!(
            a.panels[1].unloaded && a.panels[1].items.is_empty(),
            "the others when next focused"
        );
        // hide it again: rows of that repo vanish in memory, the count follows, nothing is searched
        a.panels[0].loading = false;
        a.panels[0].items = vec![gitem("e/f", 5), gitem("a/b", 1)];
        let seq = a.panels[0].seq;
        browser_on(&mut a, "e/f");
        press(&mut a, 'H');
        assert!(a.cfg.repos.is_hidden("e/f"));
        assert_eq!(a.panels[0].items.len(), 1);
        assert_eq!(
            (a.panels[0].hidden, a.panels[0].seq, a.panels[0].loading),
            (1, seq, false)
        );
        // a refused config save changes nothing
        std::fs::write(dir.join("config.toml"), "ascii = [broken\n").unwrap();
        browser_on(&mut a, "e/f");
        press(&mut a, 'H');
        assert!(a.cfg.repos.is_hidden("e/f") && a.panels[0].items.len() == 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_repo_name_that_is_not_plain_owner_name_is_shown_neutralized_and_never_used() {
        let hostile = "ev\u{202e}il/re\u{200b}po\u{2066}";
        let mut a = home();
        a.panels[0].items = vec![gitem(hostile, 7)];
        let it = a.selected().unwrap().clone();
        assert!(!it.repo_ok());
        let shown = it.repo_display();
        assert!(
            shown.contains("<U+202E>") && shown.contains("<U+200B>") && shown.contains("<U+2066>"),
            "{shown}"
        );
        assert!(!shown.contains('\u{202e}'));
        // actions: none, with a reason; the menu key, approve/merge shortcuts, checkout, comments paging
        assert!(crate::act::actions(Some(&it), 0, "", &a.sel()).is_empty());
        press(&mut a, 'x');
        assert!(
            a.modal.is_none() && a.status.contains("unsafe"),
            "{}",
            a.status
        );
        a.status.clear();
        press(&mut a, 'a');
        assert!(a.modal.is_none() && a.status.contains("unsafe"));
        press(&mut a, 'c');
        assert!(a.status.contains("unsafe"), "{}", a.status);
        // `S` refuses it too
        a.status.clear();
        press(&mut a, 'S');
        assert!(a.global, "still in the home: {}", a.status);
        // and nothing is fetched for it: the detail says why
        a.net = true;
        a.want_prev.insert((it.key(), Tab::Overview));
        a.last_input = std::time::Instant::now() - std::time::Duration::from_secs(60);
        a.fetch(it.clone(), Tab::Overview);
        assert!(
            matches!(a.cache.get(&(it.key(), Tab::Overview)), Some(Load::Done(Err(e))) if e.contains("unsafe"))
        );
        // a well-formed one is fine
        assert!(gitem("cli/cli", 1).repo_ok());
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_sent_to_gh_for_an_unsafe_repo_name() {
        let shim = crate::testshim::Shim::new();
        let mut a = home();
        a.net = true;
        a.panels[0].items = vec![gitem("--repo=evil/x", 3), gitem("../..", 4)];
        a.last_input = std::time::Instant::now() - std::time::Duration::from_secs(60);
        for _ in 0..3 {
            a.ensure();
            a.poll();
        }
        a.panels[0].cursor = 1;
        for _ in 0..3 {
            a.ensure();
        }
        quiesce();
        assert!(
            shim.calls()
                .iter()
                .all(|c| !c.contains("evil") && !c.contains("pr view") && !c.contains("pr diff")),
            "{:?}",
            shim.calls()
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_loose_state_directory_is_tightened_at_start_and_a_linked_one_refused() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let shim = crate::testshim::Shim::new();
        shim.set("searchprs.out", "[]");
        let state = shim.dir.join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
        let a = App::build_start(
            None,
            true,
            Theme::new(false, IconSet::Unicode, true),
            true,
            Config::default(),
        );
        let mode = std::fs::metadata(&state).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "tightened because it is ours");
        assert!(a.state_dir.is_some());
        // a symlink where the directory should be: refused, with a notice, and nothing is saved there
        let target = shim.dir.join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        let link = shim.dir.join("linked-state");
        symlink(&target, &link).unwrap();
        crate::state::set_dir(Some(link));
        let b = App::build_start(
            None,
            true,
            Theme::new(false, IconSet::Unicode, true),
            true,
            Config::default(),
        );
        assert!(
            b.state_dir.is_none() && b.status.starts_with("state not saved:"),
            "{}",
            b.status
        );
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
    }
}
