use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// `~/.config/gh-tui/config.toml`. Never holds credentials: auth stays with `gh`.
#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    pub ascii: bool,
    pub nerd: bool,
    /// Also mark files viewed on GitHub when you press `v` (and read GitHub's marks). Opting in is the
    /// consent for that write, so there is no extra confirm.
    pub sync_viewed: bool,
    pub repos: ReposCfg,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub keys: BTreeMap<String, Keys>,
    #[serde(skip_serializing_if = "PanelsCfg::is_default")]
    pub panels: PanelsCfg,
    #[serde(skip_serializing_if = "ApiCfg::is_default")]
    pub api: ApiCfg,
    #[serde(skip_serializing_if = "UiCfg::is_default")]
    pub ui: UiCfg,
    /// `[[sections]]`: your own search-based panels. Last, so the file stays valid TOML when saved.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<SectionCfg>,
}

/// Where gh-tui opens: the repo of the current directory, or the cross-repo home.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StartMode {
    /// A repo when the directory is a clone (or `-R` is given), otherwise the global home.
    Auto,
    /// Always a repo; outside a clone that is an error.
    Repo,
    /// Always the global home (`G` reaches the repo of the directory).
    Global,
}

impl StartMode {
    pub fn parse(s: &str) -> Option<StartMode> {
        match s {
            "auto" => Some(StartMode::Auto),
            "repo" => Some(StartMode::Repo),
            "global" => Some(StartMode::Global),
            _ => None,
        }
    }
}

/// `[ui]`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct UiCfg {
    pub start: StartMode,
}

impl Default for UiCfg {
    fn default() -> Self {
        UiCfg {
            start: StartMode::Auto,
        }
    }
}

impl UiCfg {
    fn is_default(&self) -> bool {
        *self == UiCfg::default()
    }
}

/// How tab counts are fetched: `lazy` = the focused panel's other tabs after a second of idling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Counts {
    Lazy,
    Eager,
    Off,
}

/// `[api]`: how gently gh-tui talks to GitHub.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ApiCfg {
    /// `gh` processes allowed at once (1..=8).
    pub max_concurrent: usize,
    /// Show the `⚡ remaining/limit` chip in the header below this share of the quota (percent).
    pub low_quota_percent: u8,
    /// Pause all background refreshing below this share (percent, at most `low_quota_percent`).
    pub pause_percent: u8,
    /// Show the quota chip at all.
    pub rate_header: bool,
    pub counts: Counts,
    /// Idle time before an expensive detail tab (Comments, Diff, Commits, Checks) is fetched.
    pub settle_ms: u64,
    /// Cache slow-changing lookups (labels, templates, tags, repo facts, repo list) on disk.
    pub cache: bool,
    /// Seconds a `gh` read may take before it is killed (pagination gets double, writes five times).
    pub timeout_s: u64,
}

impl Default for ApiCfg {
    fn default() -> Self {
        ApiCfg {
            max_concurrent: 4,
            low_quota_percent: 20,
            pause_percent: 10,
            rate_header: true,
            counts: Counts::Lazy,
            settle_ms: 250,
            cache: true,
            timeout_s: 60,
        }
    }
}

impl ApiCfg {
    fn is_default(&self) -> bool {
        *self == ApiCfg::default()
    }

    fn check(&self) -> Result<(), (&'static str, String)> {
        let bad = |key, m: String| Err((key, format!("api.{key}: {m}")));
        if !(1..=8).contains(&self.max_concurrent) {
            return bad(
                "max_concurrent",
                format!("must be 1..8, got {}", self.max_concurrent),
            );
        }
        if !(1..=99).contains(&self.low_quota_percent) {
            return bad("low_quota_percent", "must be 1..99".into());
        }
        if !(1..=99).contains(&self.pause_percent) || self.pause_percent > self.low_quota_percent {
            return bad(
                "pause_percent",
                format!(
                    "must be 1..99 and at most low_quota_percent ({})",
                    self.low_quota_percent
                ),
            );
        }
        if self.settle_ms > 5000 {
            return bad("settle_ms", "must be at most 5000".into());
        }
        if !(5..=600).contains(&self.timeout_s) {
            return bad("timeout_s", "must be 5..600".into());
        }
        Ok(())
    }
}

/// Left-column panels, as named in `[panels] show` (`section:<title>` is one of your `[[sections]]`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub enum PanelName {
    Prs,
    Files,
    Issues,
    Actions,
    Repo,
    Notifications,
    Status,
    /// Favorites / Recent repos (repo-mode and global).
    Repos,
    // global-home sections
    Review,
    Mine,
    Assigned,
    Involved,
    /// A `[[sections]]` entry, by title.
    Section(String),
}

impl PanelName {
    fn repo_mode(&self) -> bool {
        !matches!(
            self,
            PanelName::Review | PanelName::Mine | PanelName::Assigned | PanelName::Involved
        )
    }

    fn global_mode(&self) -> bool {
        matches!(
            self,
            PanelName::Review
                | PanelName::Mine
                | PanelName::Assigned
                | PanelName::Involved
                | PanelName::Repos
                | PanelName::Files
                | PanelName::Section(_)
        )
    }

    pub fn word(&self) -> String {
        match self {
            PanelName::Section(t) => format!("section:{t}"),
            _ => format!("{self:?}").to_lowercase(),
        }
    }
}

impl From<PanelName> for String {
    fn from(n: PanelName) -> String {
        n.word()
    }
}

impl TryFrom<String> for PanelName {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        if let Some(t) = s.strip_prefix("section:") {
            return Ok(PanelName::Section(t.to_string()));
        }
        Ok(match s.as_str() {
            "prs" => PanelName::Prs,
            "files" => PanelName::Files,
            "issues" => PanelName::Issues,
            "actions" => PanelName::Actions,
            "repo" => PanelName::Repo,
            "notifications" => PanelName::Notifications,
            "status" => PanelName::Status,
            "repos" => PanelName::Repos,
            "review" => PanelName::Review,
            "mine" => PanelName::Mine,
            "assigned" => PanelName::Assigned,
            "involved" => PanelName::Involved,
            _ => {
                return Err(format!(
                    "unknown panel {s:?}; valid: {REPO_NAMES}, review, mine, assigned, involved, section:<title>"
                ));
            }
        })
    }
}

const REPO_NAMES: &str = "prs, files, issues, actions, repo, notifications, status, repos";
const GLOBAL_NAMES: &str = "review, mine, assigned, involved, repos, files";

/// One list tab of a panel: `id` is what `gh::list` understands, `short` is the squeezed label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabDef {
    pub id: usize,
    pub label: &'static str,
    pub short: &'static str,
}

macro_rules! tab_enum {
    ($name:ident: $($v:ident $id:literal $label:literal $short:literal),+) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
        #[serde(rename_all = "lowercase")]
        pub enum $name {
            $($v),+
        }

        impl $name {
            const ALL: &[$name] = &[$($name::$v),+];

            fn def(self) -> TabDef {
                match self {
                    $($name::$v => TabDef { id: $id, label: $label, short: $short }),+
                }
            }
        }
    };
}

tab_enum!(PrTab: Mine 0 "Mine" "Mine", Review 1 "Review" "Rev", All 2 "All" "All", Merged 3 "Merged" "Mrg");
tab_enum!(IssueTab: Assigned 0 "Assigned" "Asg", Mine 1 "Mine" "Mine", All 2 "All" "All");
tab_enum!(ActionTab: Runs 0 "Runs" "Runs", Workflows 1 "Workflows" "Wf");
tab_enum!(RepoTab: Branches 0 "Branches" "Brn", Tags 1 "Tags" "Tags", Releases 2 "Releases" "Rel");

/// `[panels.<name>]`: which list tabs to show (in this order) and which one opens first.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PanelCfg<T> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tabs: Option<Vec<T>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_tab: Option<T>,
}

impl<T> Default for PanelCfg<T> {
    fn default() -> Self {
        PanelCfg {
            tabs: None,
            default_tab: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PanelsCfg {
    /// Display order = numbering. Any subset; `files` follows the PR list, so it needs `prs`.
    pub show: Vec<PanelName>,
    /// Collapse panels with nothing in any tab to one line.
    pub hide_empty: bool,
    /// The global home's panels, in display order: any of review, mine, assigned, involved, repos, files.
    pub global: Vec<PanelName>,
    pub prs: PanelCfg<PrTab>,
    pub issues: PanelCfg<IssueTab>,
    pub actions: PanelCfg<ActionTab>,
    pub repo: PanelCfg<RepoTab>,
}

impl Default for PanelsCfg {
    fn default() -> Self {
        PanelsCfg {
            show: vec![
                PanelName::Prs,
                PanelName::Files,
                PanelName::Issues,
                PanelName::Actions,
                PanelName::Repo,
            ],
            hide_empty: false,
            global: vec![
                PanelName::Review,
                PanelName::Mine,
                PanelName::Assigned,
                PanelName::Repos,
            ],
            prs: Default::default(),
            issues: Default::default(),
            actions: Default::default(),
            repo: Default::default(),
        }
    }
}

/// A panel as the app builds it.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelSpec {
    pub name: PanelName,
    pub tabs: Vec<TabDef>,
    /// Index into `tabs` that opens first.
    pub tab: usize,
}

fn resolve<T: Copy + PartialEq>(
    c: &PanelCfg<T>,
    all: &[T],
    def: impl Fn(T) -> TabDef,
) -> (Vec<TabDef>, usize) {
    let tabs = c.tabs.clone().unwrap_or_else(|| all.to_vec());
    let at = c
        .default_tab
        .and_then(|d| tabs.iter().position(|t| *t == d))
        .unwrap_or(0);
    (tabs.into_iter().map(def).collect(), at)
}

impl PanelsCfg {
    fn is_default(&self) -> bool {
        *self == PanelsCfg::default()
    }

    /// `show` plus the sections that run in a repo and are not listed, without `files` when `prs` is
    /// not shown (Files only ever describes the selected PR).
    fn effective(&self, secs: &[SectionCfg]) -> Vec<PanelName> {
        let all = with_sections(&self.show, secs, Where::in_repo);
        let prs = all.contains(&PanelName::Prs);
        all.into_iter()
            .filter(|n| prs || *n != PanelName::Files)
            .collect()
    }

    /// The global home's panels (`files` needs a PR section).
    pub fn global_layout(&self, secs: &[SectionCfg]) -> Vec<PanelSpec> {
        let td = |id, label, short| TabDef { id, label, short };
        let all = with_sections(&self.global, secs, Where::in_global);
        let prs = all.iter().any(|n| match n {
            PanelName::Review | PanelName::Mine | PanelName::Involved => true,
            PanelName::Section(t) => secs
                .iter()
                .any(|s| s.title == *t && s.kind == SectionKind::Prs),
            _ => false,
        });
        all.into_iter()
            .filter(|n| prs || *n != PanelName::Files)
            .map(|name| {
                let tabs = match name {
                    PanelName::Mine => vec![
                        td(0, "Open", "Open"),
                        td(1, "Merged", "Mrg"),
                        td(2, "Closed", "Cls"),
                    ],
                    PanelName::Assigned => vec![
                        td(0, "Assigned", "Asg"),
                        td(1, "Mine", "Mine"),
                        td(2, "Mentioned", "Ment"),
                    ],
                    PanelName::Repos => vec![td(0, "Favorites", "Fav"), td(1, "Recent", "Rec")],
                    _ => vec![],
                };
                PanelSpec { name, tabs, tab: 0 }
            })
            .collect()
    }

    pub fn layout(&self, secs: &[SectionCfg]) -> Vec<PanelSpec> {
        self.effective(secs)
            .into_iter()
            .map(|name| {
                let (tabs, tab) = match name {
                    PanelName::Prs => resolve(&self.prs, PrTab::ALL, PrTab::def),
                    PanelName::Issues => resolve(&self.issues, IssueTab::ALL, IssueTab::def),
                    PanelName::Actions => resolve(&self.actions, ActionTab::ALL, ActionTab::def),
                    PanelName::Repo => resolve(&self.repo, RepoTab::ALL, RepoTab::def),
                    _ => (vec![], 0),
                };
                PanelSpec { name, tabs, tab }
            })
            .collect()
    }

    /// (section header, key inside it, message): enough to point at the offending line.
    fn check(&self, secs: &[SectionCfg]) -> Result<(), (&'static str, &'static str, String)> {
        let section = |key: &'static str, n: &PanelName, here: fn(Where) -> bool| {
            let PanelName::Section(t) = n else {
                return Ok(());
            };
            let bad = |m: String| Err(("[panels]", key, format!("panels.{key}: {m}")));
            match secs.iter().find(|s| s.title == *t) {
                None => bad(format!(
                    "unknown section {} (defined: {})",
                    shown(t),
                    secs.iter()
                        .map(|s| shown(&s.title))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                Some(s) if !here(s.at) => bad(format!(
                    "section {} has where = \"{}\"; it cannot be listed here",
                    shown(t),
                    s.at.word()
                )),
                Some(_) => Ok(()),
            }
        };
        for n in &self.show {
            section("show", n, Where::in_repo)?;
            if !n.repo_mode() {
                return Err((
                    "[panels]",
                    "show",
                    format!(
                        "panels.show: {} is a global-home panel (list it in `global`); valid here: {REPO_NAMES}",
                        n.word()
                    ),
                ));
            }
        }
        for (i, n) in self.global.iter().enumerate() {
            section("global", n, Where::in_global)?;
            if !n.global_mode() {
                return Err((
                    "[panels]",
                    "global",
                    format!(
                        "panels.global: {} is a repo panel (list it in `show`); valid here: {GLOBAL_NAMES}",
                        n.word()
                    ),
                ));
            }
            if self.global[..i].contains(n) {
                return Err((
                    "[panels]",
                    "global",
                    format!("panels.global: {} listed twice", label(n)),
                ));
            }
        }
        if self.global_layout(secs).is_empty() {
            return Err((
                "[panels]",
                "global",
                "panels.global needs at least one panel (files only works together with review, mine or involved)".into(),
            ));
        }
        for (i, n) in self.show.iter().enumerate() {
            if self.show[..i].contains(n) {
                return Err((
                    "[panels]",
                    "show",
                    format!("panels.show: {} listed twice", label(n)),
                ));
            }
        }
        if self.effective(secs).is_empty() {
            return Err((
                "[panels]",
                "show",
                "panels.show needs at least one panel (files only works together with prs)".into(),
            ));
        }
        fn tabs<T: Copy + PartialEq + std::fmt::Debug>(
            section: &'static str,
            c: &PanelCfg<T>,
        ) -> Result<(), (&'static str, &'static str, String)> {
            let name = section.trim_matches(['[', ']']);
            let low = |t: &T| format!("{t:?}").to_lowercase();
            if let Some(v) = &c.tabs {
                if v.is_empty() {
                    return Err((
                        section,
                        "tabs",
                        format!("{name}: tabs needs at least one entry"),
                    ));
                }
                if let Some(i) = (0..v.len()).find(|&i| v[..i].contains(&v[i])) {
                    return Err((
                        section,
                        "tabs",
                        format!("{name}: {} listed twice", low(&v[i])),
                    ));
                }
            }
            if let (Some(d), Some(v)) = (&c.default_tab, &c.tabs)
                && !v.contains(d)
            {
                return Err((
                    section,
                    "default_tab",
                    format!("{name}: default_tab {} is not in tabs", low(d)),
                ));
            }
            Ok(())
        }
        tabs("[panels.prs]", &self.prs)?;
        tabs("[panels.issues]", &self.issues)?;
        tabs("[panels.actions]", &self.actions)?;
        tabs("[panels.repo]", &self.repo)
    }
}

/// `[[sections]] kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SectionKind {
    Prs,
    Issues,
}

/// `[[sections]] where`: the home(s) a section is a panel of.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Where {
    #[default]
    Global,
    Repo,
    Both,
}

impl Where {
    pub fn in_global(self) -> bool {
        self != Where::Repo
    }

    pub fn in_repo(self) -> bool {
        self != Where::Global
    }

    fn word(self) -> String {
        format!("{self:?}").to_lowercase()
    }
}

/// One `[[sections]]` entry: a GitHub search shown as a panel.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SectionCfg {
    pub title: String,
    pub kind: SectionKind,
    pub filter: String,
    /// Rows to fetch, 1..=100 (default 30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, rename = "where")]
    pub at: Where,
}

pub const MAX_SECTIONS: usize = 12;
pub const MAX_TITLE: usize = 40;
pub const MAX_FILTER: usize = 256;
pub const DEFAULT_LIMIT: usize = 30;
/// GitHub rejects search queries with more AND / OR / NOT operators than this.
pub const MAX_OPS: usize = 5;

/// A section filter split into search terms, with what the app needs to know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub terms: Vec<String>,
    /// `OR` / `AND` / `NOT` words and `-qualifier` terms: GitHub allows at most `MAX_OPS`.
    pub ops: usize,
    /// Has a `repo:` / `org:` / `user:` qualifier, so the scope is not applied.
    pub own_scope: bool,
    pub own_repo: bool,
    pub has_archived: bool,
    pub has_or: bool,
}

/// A user-written name for an error message: neutralized and quoted.
fn shown(s: &str) -> String {
    format!("{:?}", crate::sanitize::clean(s))
}

/// A panel name for an error message; section titles are user text.
fn label(n: &PanelName) -> String {
    match n {
        PanelName::Section(_) => shown(&n.word()),
        _ => n.word(),
    }
}

fn term_char_ok(c: char) -> bool {
    c.is_alphanumeric() || "-_.:/@*<>=!,~()'#+?".contains(c)
}

/// Splits on whitespace outside double quotes. Every term later travels as its own argument after
/// `--`, so nothing here is a shell or flag risk; the checks keep junk and surprises out of the query.
pub fn parse_filter(src: &str) -> Result<Filter, String> {
    if src.trim().is_empty() {
        return Err("filter is empty".into());
    }
    if src.chars().count() > MAX_FILTER {
        return Err(format!("filter is longer than {MAX_FILTER} characters"));
    }
    if let Some(c) = src.chars().find(|c| c.is_control()) {
        return Err(format!(
            "filter contains a control character (U+{:04X}); use a single line",
            c as u32
        ));
    }
    let (mut terms, mut cur, mut quoted, mut depth) = (vec![], String::new(), false, 0i32);
    for c in src.chars() {
        match c {
            // gh re-quotes a term with spaces itself (`label:good first` -> `label:"good first"`), so the
            // user's quotes only group words here and are not passed on
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    terms.push(std::mem::take(&mut cur));
                }
            }
            // parentheses outside quotes are terms of their own: glued to a quoted value they would end up
            // inside gh's quoting (`label:"a b)"`)
            '(' | ')' if !quoted => {
                if !cur.is_empty() {
                    terms.push(std::mem::take(&mut cur));
                }
                depth += i32::from(c == '(') - i32::from(c == ')');
                if depth < 0 {
                    return Err("filter has a ) without a matching (".into());
                }
                terms.push(c.to_string());
            }
            c if c.is_whitespace() || term_char_ok(c) => cur.push(c),
            c => {
                return Err(format!(
                    "filter contains {c:?}, which is not allowed in a search"
                ));
            }
        }
    }
    if quoted {
        return Err("filter has an unclosed double quote".into());
    }
    if !cur.is_empty() {
        terms.push(cur);
    }
    if depth != 0 {
        return Err("filter has a ( without a matching )".into());
    }
    if let Some(t) = terms.iter().find(|t| t.starts_with("--")) {
        return Err(format!("term {t:?} looks like a command-line flag"));
    }
    let qual = |t: &str| t.trim_start_matches('(').to_lowercase();
    let starts = |t: &str, p: &[&str]| p.iter().any(|p| qual(t).starts_with(p));
    Ok(Filter {
        ops: terms
            .iter()
            .filter(|t| {
                matches!(t.as_str(), "OR" | "AND" | "NOT")
                    || (t.starts_with('-') && t.contains(':'))
            })
            .count(),
        own_scope: terms.iter().any(|t| starts(t, &["repo:", "org:", "user:"])),
        own_repo: terms.iter().any(|t| starts(t, &["repo:"])),
        has_archived: terms.iter().any(|t| {
            let q = qual(t);
            let q = q.trim_start_matches('-');
            q.starts_with("archived:") || q == "is:archived"
        }),
        has_or: terms.iter().any(|t| t == "OR"),
        terms,
    })
}

impl SectionCfg {
    pub fn limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_LIMIT)
    }

    /// (key, message) of the first problem.
    fn check(&self) -> Result<(), (&'static str, String)> {
        // display width of what the panel will show, not the raw text
        let shown = crate::sanitize::clean(&self.title);
        let n = unicode_width::UnicodeWidthStr::width(shown.as_ref());
        if shown.trim().is_empty() || n > MAX_TITLE {
            return Err((
                "title",
                format!(
                    "sections.title: must be 1..{MAX_TITLE} columns wide and not blank, got {n}"
                ),
            ));
        }
        if !self.limit.is_none_or(|l| (1..=100).contains(&l)) {
            return Err(("limit", "sections.limit: must be 1..100".into()));
        }
        parse_filter(&self.filter)
            .map(|_| ())
            .map_err(|e| ("filter", format!("sections.filter: {e}")))
    }
}

/// Soft problems worth telling the user about at startup (the file still loads).
pub fn warnings(cfg: &Config) -> Vec<String> {
    cfg.sections
        .iter()
        .filter_map(|s| {
            let f = parse_filter(&s.filter).ok()?;
            (f.ops > MAX_OPS).then(|| {
                format!(
                    "section {:?}: {} AND/OR/NOT operators; GitHub allows {MAX_OPS}, the search may fail",
                    crate::sanitize::clean(&s.title),
                    f.ops
                )
            })
        })
        .collect()
}

fn with_sections(
    listed: &[PanelName],
    secs: &[SectionCfg],
    here: fn(Where) -> bool,
) -> Vec<PanelName> {
    let mut v = listed.to_vec();
    for s in secs.iter().filter(|s| here(s.at)) {
        let n = PanelName::Section(s.title.clone());
        if !v.contains(&n) {
            v.push(n);
        }
    }
    v
}

fn check_sections(secs: &[SectionCfg]) -> Result<(), (usize, &'static str, String)> {
    for (i, s) in secs.iter().enumerate() {
        if i >= MAX_SECTIONS {
            return Err((
                i,
                "title",
                format!("sections: at most {MAX_SECTIONS} sections"),
            ));
        }
        s.check().map_err(|(k, m)| (i, k, m))?;
        let clean = |t: &str| crate::sanitize::clean(t).into_owned();
        if secs[..i].iter().any(|o| clean(&o.title) == clean(&s.title)) {
            return Err((
                i,
                "title",
                format!("sections.title: {} is used twice", shown(&s.title)),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ReposCfg {
    pub favorites: Vec<String>,
    pub hidden: Vec<String>,
}

impl ReposCfg {
    pub fn is_fav(&self, r: &str) -> bool {
        self.favorites.iter().any(|f| f.eq_ignore_ascii_case(r))
    }

    pub fn is_hidden(&self, r: &str) -> bool {
        self.hidden.iter().any(|f| f.eq_ignore_ascii_case(r))
    }

    /// Adds when absent, removes when present; returns the new state.
    fn toggle(list: &mut Vec<String>, r: &str) -> bool {
        match list.iter().position(|f| f.eq_ignore_ascii_case(r)) {
            Some(i) => {
                list.remove(i);
                false
            }
            None => {
                list.push(r.to_string());
                true
            }
        }
    }

    pub fn toggle_fav(&mut self, r: &str) -> bool {
        Self::toggle(&mut self.favorites, r)
    }

    pub fn toggle_hidden(&mut self, r: &str) -> bool {
        Self::toggle(&mut self.hidden, r)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Keys {
    One(String),
    Many(Vec<String>),
}

impl Keys {
    fn list(&self) -> Vec<&str> {
        match self {
            Keys::One(s) => vec![s],
            Keys::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
    Some(crate::paths::data_dir(&base).join("config.toml"))
}

/// 1-based line of `key` inside the `nth` table headed `header`; the header's line if the key is implied.
fn key_line(src: &str, header: &str, nth: usize, key: &str) -> usize {
    let lines: Vec<&str> = src.lines().collect();
    let Some(h) = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim() == header)
        .nth(nth)
        .map(|(i, _)| i)
    else {
        return 1;
    };
    lines[h + 1..]
        .iter()
        .take_while(|l| !l.trim_start().starts_with('['))
        .position(|l| l.trim_start().starts_with(key))
        .map_or(h + 1, |k| h + 2 + k)
}

pub fn parse(src: &str, name: &str) -> Result<Config, String> {
    let cfg: Config = toml::from_str(src).map_err(|e| {
        let line = e.span().map_or(1, |s| {
            src[..s.start.min(src.len())].matches('\n').count() + 1
        });
        format!("{name}:{line}: {}", e.message().trim())
    })?;
    if let Some(t) = &cfg.theme
        && !matches!(t.as_str(), "dark" | "light")
    {
        let line = src
            .lines()
            .position(|l| l.trim_start().starts_with("theme"))
            .map_or(1, |i| i + 1);
        return Err(format!(
            "{name}:{line}: theme must be \"dark\" or \"light\", got {t:?}"
        ));
    }
    if let Err((i, key, msg)) = check_sections(&cfg.sections) {
        return Err(format!(
            "{name}:{}: {msg}",
            key_line(src, "[[sections]]", i, key)
        ));
    }
    if let Err((section, key, msg)) = cfg.panels.check(&cfg.sections) {
        return Err(format!("{name}:{}: {msg}", key_line(src, section, 0, key)));
    }
    if let Err((key, msg)) = cfg.api.check() {
        return Err(format!("{name}:{}: {msg}", key_line(src, "[api]", 0, key)));
    }
    Ok(cfg)
}

/// A missing file means defaults; an unreadable or invalid one is an error.
pub fn load_from(path: &Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => parse(&s, &path.display().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Write to a temp file in the same directory, then rename over the target. Refuses to overwrite a
/// file that no longer parses (the user may be mid-edit): their text is never clobbered.
pub fn save_to(path: &Path, cfg: &Config) -> Result<(), String> {
    load_from(path).map_err(|e| format!("not saving, config has errors: {e}"))?;
    let dir = path.parent().ok_or("bad config path")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let body = toml::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Named normal-mode actions whose keys can be remapped under `[keys]`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Act {
    Quit,
    Help,
    Refresh,
    RefreshAll,
    Actions,
    Approve,
    Comment,
    Merge,
    Open,
    CopyUrl,
    Checkout,
    CommandLog,
    Filter,
    Zoom,
    Global,
    Browser,
    Inbox,
    Scope,
    SwitchRepoContext,
}

impl Act {
    pub const ALL: [Act; 19] = [
        Act::Quit,
        Act::Help,
        Act::Refresh,
        Act::RefreshAll,
        Act::Actions,
        Act::Approve,
        Act::Comment,
        Act::Merge,
        Act::Open,
        Act::CopyUrl,
        Act::Checkout,
        Act::CommandLog,
        Act::Filter,
        Act::Zoom,
        Act::Global,
        Act::Browser,
        Act::Inbox,
        Act::Scope,
        Act::SwitchRepoContext,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Act::Quit => "quit",
            Act::Help => "help",
            Act::Refresh => "refresh",
            Act::RefreshAll => "refresh_all",
            Act::Actions => "actions",
            Act::Approve => "approve",
            Act::Comment => "comment",
            Act::Merge => "merge",
            Act::Open => "open",
            Act::CopyUrl => "copy_url",
            Act::Checkout => "checkout",
            Act::CommandLog => "command_log",
            Act::Filter => "filter",
            Act::Zoom => "zoom",
            Act::Global => "global",
            Act::Browser => "repo_browser",
            Act::Inbox => "inbox",
            Act::Scope => "scope",
            Act::SwitchRepoContext => "switch_repo_context",
        }
    }

    fn default_keys(self) -> &'static [&'static str] {
        match self {
            Act::Quit => &["q"],
            Act::Help => &["?"],
            Act::Refresh => &["r"],
            Act::RefreshAll => &["R"],
            Act::Actions => &["x"],
            Act::Approve => &["a"],
            Act::Comment => &["C"],
            Act::Merge => &["m"],
            Act::Open => &["o"],
            Act::CopyUrl => &["y"],
            Act::Checkout => &["c"],
            Act::CommandLog => &["L"],
            Act::Filter => &["/"],
            Act::Zoom => &["f"],
            Act::Global => &["G"],
            Act::Browser => &["B", "ctrl-r"],
            Act::Inbox => &["N"],
            Act::Scope => &["s"],
            Act::SwitchRepoContext => &["S"],
        }
    }
}

type KeyId = (KeyCode, bool);

/// Keys that navigate or belong to a context, so they can't be given to an action.
fn reserved(k: KeyId) -> bool {
    match k {
        (KeyCode::Char(c), false) => "jkhlgnpvtwesSTH.[]{}1234567".contains(c),
        (KeyCode::Char('c' | 'd' | 'u'), true) => true,
        (KeyCode::Char(_), _) => false,
        (_, _) => matches!(
            k.0,
            KeyCode::Enter
                | KeyCode::Esc
                | KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
        ),
    }
}

pub fn parse_key(s: &str) -> Result<KeyId, String> {
    let l = s.to_lowercase();
    if let Some(rest) = l.strip_prefix("ctrl-").or_else(|| l.strip_prefix("c-")) {
        let mut cs = rest.chars();
        return match (cs.next(), cs.next()) {
            (Some(c), None) => Ok((KeyCode::Char(c), true)),
            _ => Err(format!("bad key {s:?}: expected ctrl-<letter>")),
        };
    }
    let named = match l.as_str() {
        "tab" => Some(KeyCode::Tab),
        "backtab" | "s-tab" | "shift-tab" => Some(KeyCode::BackTab),
        "enter" | "return" => Some(KeyCode::Enter),
        "esc" | "escape" => Some(KeyCode::Esc),
        "space" => Some(KeyCode::Char(' ')),
        "backspace" => Some(KeyCode::Backspace),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        _ => None,
    };
    if let Some(k) = named {
        return Ok((k, false));
    }
    let mut cs = s.chars();
    match (cs.next(), cs.next()) {
        (Some(c), None) => Ok((KeyCode::Char(c), false)),
        _ => Err(format!(
            "unknown key {s:?} (single character, ctrl-<letter>, or tab/enter/esc/space/...)"
        )),
    }
}

pub struct Keymap {
    by_key: HashMap<KeyId, Act>,
    shown: HashMap<Act, Vec<String>>,
}

impl Keymap {
    /// Defaults plus `[keys]` overrides (an override replaces that action's default keys).
    /// Unknown actions, unparsable keys, reserved keys and clashes are errors.
    pub fn build(over: &BTreeMap<String, Keys>) -> Result<Keymap, String> {
        let mut want: HashMap<Act, Vec<String>> = Act::ALL
            .iter()
            .map(|a| (*a, a.default_keys().iter().map(|s| s.to_string()).collect()))
            .collect();
        for (name, keys) in over {
            let act = Act::ALL.iter().find(|a| a.name() == name).ok_or_else(|| {
                let all: Vec<_> = Act::ALL.iter().map(|a| a.name()).collect();
                format!(
                    "[keys]: unknown action {name:?} (valid: {})",
                    all.join(", ")
                )
            })?;
            let list: Vec<String> = keys.list().iter().map(|s| s.to_string()).collect();
            if list.is_empty() {
                return Err(format!("[keys] {name}: give at least one key"));
            }
            want.insert(*act, list);
        }
        let (mut by_key, mut owner): (HashMap<KeyId, Act>, HashMap<KeyId, &str>) =
            Default::default();
        for act in Act::ALL {
            for k in &want[&act] {
                let id = parse_key(k).map_err(|e| format!("[keys] {}: {e}", act.name()))?;
                // the shipped defaults include keys that double as navigation (G), so only user keys are policed
                let user = over.contains_key(act.name());
                if user && reserved(id) {
                    return Err(format!(
                        "[keys] {}: {k:?} is a built-in navigation key and can't be remapped",
                        act.name()
                    ));
                }
                if let Some(prev) = owner.insert(id, act.name()) {
                    return Err(format!(
                        "[keys] {k:?} is bound to both {prev} and {}",
                        act.name()
                    ));
                }
                by_key.insert(id, act);
            }
        }
        Ok(Keymap {
            by_key,
            shown: want,
        })
    }

    pub fn get(&self, k: &KeyEvent) -> Option<Act> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.by_key.get(&(k.code, ctrl)).copied()
    }

    /// All keys bound to the action ("B / ctrl-r"), for the help screen.
    pub fn labels(&self, a: Act) -> String {
        self.shown
            .get(&a)
            .map(|v| v.join(" / "))
            .unwrap_or_default()
    }

    /// First key bound to the action, for hints.
    pub fn label(&self, a: Act) -> String {
        self.shown
            .get(&a)
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_toml_examples_parse() {
        for file in ["README.md", "docs/configuration.md"] {
            let text =
                std::fs::read_to_string(format!("{}/{file}", env!("CARGO_MANIFEST_DIR"))).unwrap();
            let mut n = 0;
            for block in text.split("```toml\n").skip(1) {
                let block = block.split("```").next().unwrap();
                if block.contains("# partial") {
                    continue;
                }
                n += 1;
                parse(block, file).unwrap_or_else(|e| panic!("{e}\n{block}"));
            }
            assert!(n > 0, "{file}: no toml blocks found");
        }
    }

    fn keys(toml: &str) -> BTreeMap<String, Keys> {
        parse(toml, "t").unwrap().keys
    }

    #[test]
    fn defaults_and_full_file() {
        assert_eq!(parse("", "t").unwrap(), Config::default());
        let c = parse(
            "theme = \"light\"\nascii = true\n[repos]\nfavorites = [\"o/a\"]\nhidden = [\"o/b\"]\n[keys]\nquit = [\"q\", \"ctrl-q\"]\nactions = \"space\"\n",
            "t",
        )
        .unwrap();
        assert_eq!(c.theme.as_deref(), Some("light"));
        assert!(c.ascii && !c.nerd);
        assert!(!c.sync_viewed, "opt-in, off by default");
        assert!(parse("sync_viewed = true\n", "t").unwrap().sync_viewed);
        assert!(c.repos.is_fav("O/A") && c.repos.is_hidden("o/b"));
        assert_eq!(c.keys.len(), 2);
    }

    #[test]
    fn invalid_files_name_the_line() {
        let e = parse("ascii = true\nnerd = 3\n", "cfg").unwrap_err();
        assert!(e.starts_with("cfg:2:"), "{e}");
        let e = parse("ascii = true\ntheem = \"dark\"\n", "cfg").unwrap_err();
        assert!(e.starts_with("cfg:2:") && e.contains("theem"), "{e}");
        let e = parse("a = 1\n\ntheme = \"neon\"\n", "cfg");
        assert!(e.is_err());
        let e = parse("theme = \"neon\"\n", "cfg").unwrap_err();
        assert!(e.starts_with("cfg:1:") && e.contains("neon"), "{e}");
    }

    #[test]
    fn key_overrides_resolve_and_conflicts_fail() {
        let km = Keymap::build(&BTreeMap::new()).unwrap();
        let ev = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        assert_eq!(km.get(&ev('x')), Some(Act::Actions));
        assert_eq!(
            km.get(&KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            Some(Act::Browser)
        );
        assert_eq!(km.label(Act::Quit), "q");
        let km = Keymap::build(&keys(
            "[keys]\nactions = \"z\"\nquit = [\"Q\", \"ctrl-q\"]\n",
        ))
        .unwrap();
        assert_eq!(km.get(&ev('z')), Some(Act::Actions));
        assert_eq!(km.get(&ev('x')), None, "override replaces the default");
        assert_eq!(km.get(&ev('Q')), Some(Act::Quit));
        assert_eq!(km.label(Act::Quit), "Q");
        let err = |t: &str| Keymap::build(&keys(t)).err().unwrap_or_default();
        assert!(err("[keys]\nnope = \"z\"\n").contains("unknown action"));
        assert!(err("[keys]\nactions = \"j\"\n").contains("navigation"));
        assert!(
            err("[keys]\nactions = \"q\"\n").contains("both"),
            "clash with quit"
        );
        assert!(err("[keys]\nactions = \"abc\"\n").contains("unknown key"));
        assert!(err("[keys]\nactions = []\n").contains("at least one"));
    }

    #[test]
    fn atomic_save_round_trips_and_never_clobbers() {
        let dir = std::env::temp_dir().join(format!("gh-tui-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("config.toml");
        assert_eq!(
            load_from(&path).unwrap(),
            Config::default(),
            "missing file = defaults"
        );
        let mut c = Config {
            theme: Some("dark".into()),
            nerd: true,
            ..Default::default()
        };
        c.repos.toggle_fav("o/a");
        c.repos.toggle_hidden("o/b");
        c.keys
            .insert("quit".into(), Keys::Many(vec!["q".into(), "ctrl-q".into()]));
        save_to(&path, &c).unwrap();
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "temp file renamed away"
        );
        assert_eq!(load_from(&path).unwrap(), c);
        assert!(
            !c.repos.clone().toggle_fav("o/a"),
            "toggle removes when present"
        );
        std::fs::write(&path, "ascii = [oops\n").unwrap();
        assert!(save_to(&path, &c).unwrap_err().contains("not saving"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "ascii = [oops\n",
            "invalid file left untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn names(c: &Config) -> Vec<PanelName> {
        c.panels
            .layout(&[])
            .iter()
            .map(|p| p.name.clone())
            .collect()
    }

    #[test]
    fn panels_default_to_the_five_panel_layout() {
        use PanelName::*;
        let c = parse("", "t").unwrap();
        assert_eq!(names(&c), [Prs, Files, Issues, Actions, Repo]);
        let tabs: Vec<_> = c.panels.layout(&[]).iter().map(|p| p.tabs.len()).collect();
        assert_eq!(tabs, [4, 0, 3, 2, 3]);
        assert!(!c.panels.hide_empty);
        // the inbox key is part of the closed key set
        assert_eq!(Act::ALL.len(), 19);
        assert_eq!(
            Keymap::build(&BTreeMap::new()).unwrap().label(Act::Inbox),
            "N"
        );
    }

    #[test]
    fn panels_order_tabs_and_default_tab_are_configurable() {
        use PanelName::*;
        let c = parse(
            "[panels]\nshow = [\"repo\", \"prs\", \"issues\"]\nhide_empty = true\n\
             [panels.repo]\ntabs = [\"releases\", \"branches\"]\ndefault_tab = \"branches\"\n\
             [panels.prs]\ndefault_tab = \"review\"\n",
            "t",
        )
        .unwrap();
        assert_eq!(names(&c), [Repo, Prs, Issues], "order = numbering");
        let l = c.panels.layout(&[]);
        assert_eq!(l[0].tabs.iter().map(|t| t.id).collect::<Vec<_>>(), [2, 0]);
        assert_eq!(l[0].tab, 1, "default_tab picks its position in tabs");
        assert_eq!((l[1].tabs[l[1].tab].label, l[1].tabs.len()), ("Review", 4));
        assert!(c.panels.hide_empty);
        // files needs prs
        let c = parse("[panels]\nshow = [\"files\", \"issues\"]\n", "t").unwrap();
        assert_eq!(names(&c), [Issues]);
        // the optional panels are available behind config
        let c = parse(
            "[panels]\nshow = [\"status\", \"notifications\", \"prs\"]\n",
            "t",
        )
        .unwrap();
        assert_eq!(names(&c), [Status, Notifications, Prs]);
    }

    #[test]
    fn invalid_panels_name_the_line_and_the_valid_choices() {
        let e = parse(
            "ascii = true\n[panels]\nshow = [\"prs\", \"nope\"]\n",
            "cfg",
        )
        .unwrap_err();
        assert!(
            e.starts_with("cfg:3:")
                && e.contains("nope")
                && e.contains("repo")
                && e.contains("issues"),
            "{e}"
        );
        let e = parse("[panels]\nshow = [\"prs\", \"prs\"]\n", "cfg").unwrap_err();
        assert!(
            e.starts_with("cfg:2:") && e.contains("prs listed twice") && !e.contains("Prs"),
            "{e}"
        );
        let e = parse("[panels]\nshow = []\n", "cfg").unwrap_err();
        assert!(e.starts_with("cfg:2:") && e.contains("at least one"), "{e}");
        let e = parse("[panels]\nshow = [\"files\"]\n", "cfg").unwrap_err();
        assert!(
            e.contains("at least one") && e.contains("prs"),
            "files alone is nothing: {e}"
        );
        let e = parse("[panels.repo]\ntabs = [\"branches\", \"nope\"]\n", "cfg").unwrap_err();
        assert!(e.starts_with("cfg:2:") && e.contains("tags"), "{e}");
        let e = parse("[panels.repo]\ntabs = []\n", "cfg").unwrap_err();
        assert!(
            e.starts_with("cfg:2:") && e.contains("panels.repo: tabs needs"),
            "{e}"
        );
        let e = parse("[panels.prs]\ntabs = [\"mine\", \"mine\"]\n", "cfg").unwrap_err();
        assert!(
            e.starts_with("cfg:2:") && e.contains("panels.prs: mine listed twice"),
            "{e}"
        );
        assert!(!e.contains("panels.panels"), "{e}");
        let e = parse(
            "[panels.issues]\ntabs = [\"all\"]\ndefault_tab = \"mine\"\n",
            "cfg",
        )
        .unwrap_err();
        assert!(
            e.starts_with("cfg:3:") && e.contains("panels.issues: default_tab mine is not in tabs"),
            "points at the default_tab line: {e}"
        );
        let e = parse("[panels]\nsho = []\n", "cfg").unwrap_err();
        assert!(e.starts_with("cfg:2:") && e.contains("sho"), "{e}");
    }

    #[test]
    fn panels_round_trip_through_save() {
        let dir = std::env::temp_dir().join(format!("gh-tui-panels-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");
        let mut c = parse(
            "[panels]\nshow = [\"repo\", \"prs\"]\nhide_empty = true\n[panels.repo]\ntabs = [\"tags\"]\n",
            "t",
        )
        .unwrap();
        c.repos.toggle_fav("o/a");
        save_to(&path, &c).unwrap();
        assert_eq!(load_from(&path).unwrap(), c);
        // a default layout is not written back
        let d = Config {
            nerd: true,
            ..Default::default()
        };
        save_to(&path, &d).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("panels"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn api_section_has_gentle_defaults_and_validates_ranges() {
        let c = parse("", "t").unwrap();
        assert_eq!(c.api, ApiCfg::default());
        assert_eq!(
            (
                c.api.max_concurrent,
                c.api.low_quota_percent,
                c.api.pause_percent,
                c.api.settle_ms
            ),
            (4, 20, 10, 250)
        );
        assert!(c.api.rate_header && c.api.cache && c.api.counts == Counts::Lazy);
        let c = parse(
            "[api]\nmax_concurrent = 8\ncounts = \"eager\"\nsettle_ms = 0\ncache = false\nrate_header = false\nlow_quota_percent = 30\npause_percent = 30\n",
            "t",
        )
        .unwrap();
        assert_eq!(
            (c.api.max_concurrent, c.api.counts, c.api.cache),
            (8, Counts::Eager, false)
        );
        for (src, line, want) in [
            (
                "[api]\nmax_concurrent = 0\n",
                2,
                "max_concurrent: must be 1..8",
            ),
            ("[api]\nmax_concurrent = 9\n", 2, "1..8"),
            (
                "[api]\nx = 1\nlow_quota_percent = 100\n",
                2,
                "unknown field",
            ),
            ("[api]\ncounts = \"sometimes\"\n", 2, "lazy"),
            (
                "[api]\nlow_quota_percent = 10\npause_percent = 15\n",
                3,
                "pause_percent: must be 1..99 and at most low_quota_percent (10)",
            ),
            ("[api]\nsettle_ms = 9000\n", 2, "settle_ms"),
            ("[api]\ntimeout_s = 1\n", 2, "timeout_s: must be 5..600"),
        ] {
            let e = parse(src, "cfg").unwrap_err();
            assert!(
                e.starts_with(&format!("cfg:{line}:")) && e.contains(want),
                "{src:?}: {e}"
            );
        }
        // a non-default section survives the app's own saves; the default one is not written
        let dir = std::env::temp_dir().join(format!("gh-tui-api-{}", std::process::id()));
        let path = dir.join("config.toml");
        let c = parse("[api]\nmax_concurrent = 2\n", "t").unwrap();
        save_to(&path, &c).unwrap();
        assert_eq!(load_from(&path).unwrap(), c);
        save_to(&path, &Config::default()).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("api"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ui_start_defaults_to_auto_and_rejects_unknown_modes() {
        assert_eq!(parse("", "t").unwrap().ui.start, StartMode::Auto);
        for (v, m) in [
            ("auto", StartMode::Auto),
            ("repo", StartMode::Repo),
            ("global", StartMode::Global),
        ] {
            assert_eq!(
                parse(&format!("[ui]\nstart = \"{v}\"\n"), "t")
                    .unwrap()
                    .ui
                    .start,
                m
            );
        }
        let e = parse("ascii = true\n[ui]\nstart = \"home\"\n", "cfg").unwrap_err();
        assert!(
            e.starts_with("cfg:3:") && e.contains("auto") && e.contains("global"),
            "{e}"
        );
        let e = parse("[ui]\nstat = \"auto\"\n", "cfg").unwrap_err();
        assert!(e.starts_with("cfg:2:") && e.contains("stat"), "{e}");
        // a non-default value survives the app's own saves; the default is not written
        let dir = std::env::temp_dir().join(format!("gh-tui-ui-{}", std::process::id()));
        let path = dir.join("config.toml");
        let c = parse("[ui]\nstart = \"global\"\n", "t").unwrap();
        save_to(&path, &c).unwrap();
        assert_eq!(load_from(&path).unwrap(), c);
        save_to(&path, &Config::default()).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("[ui]"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_global_home_panels_are_configurable_and_validated() {
        use PanelName::*;
        let g = |c: &Config| -> Vec<PanelName> {
            c.panels
                .global_layout(&[])
                .iter()
                .map(|p| p.name.clone())
                .collect()
        };
        let c = parse("", "t").unwrap();
        assert_eq!(
            g(&c),
            [Review, Mine, Assigned, Repos],
            "Involved is off by default"
        );
        let tabs: Vec<_> = c
            .panels
            .global_layout(&[])
            .iter()
            .map(|p| p.tabs.len())
            .collect();
        assert_eq!(tabs, [0, 3, 3, 2]);
        let c = parse(
            "[panels]\nglobal = [\"repos\", \"involved\", \"review\", \"files\"]\n",
            "t",
        )
        .unwrap();
        assert_eq!(g(&c), [Repos, Involved, Review, Files]);
        // files needs a PR section
        let c = parse("[panels]\nglobal = [\"assigned\", \"files\"]\n", "t").unwrap();
        assert_eq!(g(&c), [Assigned]);
        // repo-mode lists may now include `repos`, but not the global sections (and vice versa)
        let c = parse("[panels]\nshow = [\"prs\", \"repos\"]\n", "t").unwrap();
        assert_eq!(
            c.panels
                .layout(&[])
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>(),
            [Prs, Repos]
        );
        let e = parse("[panels]\nshow = [\"prs\", \"review\"]\n", "cfg").unwrap_err();
        assert!(
            e.starts_with("cfg:2:") && e.contains("global-home panel") && e.contains("repos"),
            "{e}"
        );
        let e = parse("[panels]\nglobal = [\"review\", \"actions\"]\n", "cfg").unwrap_err();
        assert!(
            e.starts_with("cfg:2:") && e.contains("repo panel") && e.contains("involved"),
            "{e}"
        );
        let e = parse("[panels]\nglobal = [\"mine\", \"mine\"]\n", "cfg").unwrap_err();
        assert!(e.contains("mine listed twice"), "{e}");
        let e = parse("[panels]\nglobal = []\n", "cfg").unwrap_err();
        assert!(e.starts_with("cfg:2:") && e.contains("at least one"), "{e}");
        let e = parse("[panels]\nglobal = [\"nope\"]\n", "cfg").unwrap_err();
        assert!(
            e.starts_with("cfg:2:") && e.contains("nope") && e.contains("review"),
            "{e}"
        );
        // the new actions are part of the closed key set with their defaults
        let km = Keymap::build(&BTreeMap::new()).unwrap();
        assert_eq!(
            (km.label(Act::Scope), km.label(Act::SwitchRepoContext)),
            ("s".into(), "S".into())
        );
        let mut over = BTreeMap::new();
        over.insert("scope".to_string(), Keys::One("z".into()));
        assert_eq!(Keymap::build(&over).unwrap().label(Act::Scope), "z");
    }
    const TWO: &str = r#"
[[sections]]
title  = "Needs my review (acme)"
kind   = "prs"
filter = "is:open review-requested:@me org:acme draft:false"
limit  = 50
where  = "both"

[[sections]]
title  = "Bugs"
kind   = "issues"
filter = 'is:open label:"good first issue" -label:wip'
"#;

    #[test]
    fn sections_parse_with_defaults_and_survive_a_save() {
        let c = parse(TWO, "cfg").unwrap();
        assert_eq!(c.sections.len(), 2);
        let (a, b) = (&c.sections[0], &c.sections[1]);
        assert_eq!(
            (a.kind, a.at, a.limit()),
            (SectionKind::Prs, Where::Both, 50)
        );
        assert_eq!(
            (b.kind, b.at, b.limit()),
            (SectionKind::Issues, Where::Global, 30)
        );
        // quotes group words and are not passed on; `-label:wip` counts as an operator
        let f = parse_filter(&b.filter).unwrap();
        assert_eq!(f.terms, ["is:open", "label:good first issue", "-label:wip"]);
        assert_eq!((f.ops, f.own_scope, f.has_archived), (1, false, false));
        // saved files re-read to the same sections
        let back = parse(&toml::to_string_pretty(&c).unwrap(), "cfg").unwrap();
        assert_eq!(back.sections, c.sections);
        // sections are extra panels after the built-in ones, per home
        let names = |l: Vec<PanelSpec>| l.into_iter().map(|p| p.name.word()).collect::<Vec<_>>();
        assert_eq!(
            names(c.panels.global_layout(&c.sections)),
            [
                "review",
                "mine",
                "assigned",
                "repos",
                "section:Needs my review (acme)",
                "section:Bugs"
            ]
        );
        assert_eq!(
            names(c.panels.layout(&c.sections))
                .last()
                .map(String::as_str),
            Some("section:Needs my review (acme)"),
            "only where = repo / both in the repo home"
        );
    }

    #[test]
    fn panels_lists_place_sections_and_unknown_names_are_errors() {
        let src = format!(
            "{TWO}\n[panels]\nglobal = [\"section:Bugs\", \"review\"]\nshow = [\"section:Needs my review (acme)\", \"prs\"]\n"
        );
        let c = parse(&src, "cfg").unwrap();
        let g: Vec<_> = c
            .panels
            .global_layout(&c.sections)
            .iter()
            .map(|p| p.name.word())
            .collect();
        assert_eq!(
            g,
            ["section:Bugs", "review", "section:Needs my review (acme)"]
        );
        let e = parse("[[sections]]\ntitle=\"A\"\nkind=\"prs\"\nfilter=\"x\"\n[panels]\nglobal = [\"section:B\"]\n", "cfg").unwrap_err();
        assert!(
            e.starts_with("cfg:6:") && e.contains("unknown section \"B\"") && e.contains("A"),
            "{e}"
        );
        let e = parse("[panels]\nshow = [\"section:B\"]\n", "cfg").unwrap_err();
        assert!(
            e.starts_with("cfg:2:") && e.contains("unknown section"),
            "{e}"
        );
        // a global-only section cannot be placed in the repo home
        let e = parse("[[sections]]\ntitle=\"A\"\nkind=\"prs\"\nfilter=\"x\"\n[panels]\nshow = [\"prs\", \"section:A\"]\n", "cfg").unwrap_err();
        assert!(
            e.starts_with("cfg:6:") && e.contains("where = \"global\""),
            "{e}"
        );
    }

    #[test]
    fn bad_sections_are_errors_naming_the_line() {
        let sec = |body: &str| {
            format!("# top\n[[sections]]\ntitle = \"A\"\nkind = \"prs\"\nfilter = \"x\"\n{body}")
        };
        let bad = |src: String| parse(&src, "cfg").unwrap_err();
        let e = bad(sec("").replace("\"prs\"", "\"repos\""));
        assert!(
            e.starts_with("cfg:4:") && e.contains("prs") && e.contains("issues"),
            "{e}"
        );
        let e = bad(sec("limit = 0\n"));
        assert!(e.starts_with("cfg:6:") && e.contains("1..100"), "{e}");
        let e = bad(sec("limit = 101\n"));
        assert!(e.contains("1..100"), "{e}");
        let e = bad(sec("where = \"nowhere\"\n"));
        assert!(e.starts_with("cfg:6:") && e.contains("nowhere"), "{e}");
        let e = bad(sec("").replace("\"x\"", &format!("\"{}\"", "a".repeat(257))));
        assert!(e.starts_with("cfg:5:") && e.contains("256"), "{e}");
        for evil in ["a\\nb", "a\\u0007b", "a\\tb"] {
            let e = bad(sec("").replace("\"x\"", &format!("\"{evil}\"")));
            assert!(
                e.starts_with("cfg:5:") && e.contains("control character"),
                "{evil}: {e}"
            );
        }
        for evil in [
            "$(x)", "a;b", "`x`", "a|b", "a&b", "a\\\\b", "--web", "x --jq=.", "\\\"open",
        ] {
            let e = bad(sec("").replace("\"x\"", &format!("\"{evil}\"")));
            assert!(
                e.starts_with("cfg:5:") && e.contains("filter"),
                "{evil}: {e}"
            );
        }
        let e = bad(sec("").replace("\"x\"", "\" \""));
        assert!(e.contains("empty"), "{e}");
        let e = bad(sec("").replace("\"A\"", &format!("\"{}\"", "t".repeat(41))));
        assert!(e.starts_with("cfg:3:") && e.contains("1..40"), "{e}");
        let e = bad(format!("{}{}", sec(""), sec("")));
        assert!(e.starts_with("cfg:8:") && e.contains("used twice"), "{e}");
        let many: String = (0..13)
            .map(|i| format!("[[sections]]\ntitle = \"s{i}\"\nkind = \"prs\"\nfilter = \"x\"\n"))
            .collect();
        let e = bad(many);
        assert!(e.contains("at most 12"), "{e}");
        let e = bad("[[sections]]\ntitle = \"A\"\n".into());
        assert!(e.contains("missing field"), "{e}");
    }

    #[test]
    fn operator_counts_and_scope_markers_come_from_the_terms() {
        let f = |s: &str| parse_filter(s).unwrap();
        assert_eq!(f("a OR b AND c NOT d -label:x").ops, 4);
        assert_eq!(f("(repo:a/b OR repo:c/d)").ops, 1);
        assert!(f("(repo:a/b OR repo:c/d)").own_scope && f("USER:me").own_scope);
        assert!(!f("-org:x").own_scope && !f("label:org:x").own_scope);
        assert!(f("-archived:true").has_archived && f("archived:false").has_archived);
        // too many operators is a warning, not an error
        let src = "[[sections]]\ntitle = \"A\"\nkind = \"prs\"\nfilter = \"a OR b OR c OR d OR e OR f OR g\"\n";
        let c = parse(src, "cfg").unwrap();
        let w = warnings(&c);
        assert!(w.len() == 1 && w[0].contains("6 AND/OR/NOT"), "{w:?}");
        assert!(warnings(&parse(TWO, "cfg").unwrap()).is_empty());
    }
    #[test]
    fn parentheses_archived_and_titles_follow_the_audit_rules() {
        let one = |filter: &str, title: &str| {
            format!("[[sections]]\ntitle = '{title}'\nkind = \"prs\"\nfilter = {filter:?}\n")
        };
        for bad in ["(a OR b", "a OR b)", ")a(", "((a)"] {
            let e = parse(&one(bad, "A"), "cfg").unwrap_err();
            assert!(
                e.starts_with("cfg:4:") && e.contains("matching"),
                "{bad}: {e}"
            );
        }
        // parentheses inside quotes are text, not groups
        assert!(parse(&one("\"fix (x\" OR y", "A"), "cfg").is_ok());
        let f = parse_filter(r#"(label:"a b" OR c)"#).unwrap();
        assert_eq!(f.terms, ["(", "label:a b", "OR", "c", ")"]);
        assert!(parse_filter("is:archived x").unwrap().has_archived);
        assert!(parse_filter("-is:archived x").unwrap().has_archived);
        assert!(!parse_filter("is:open").unwrap().has_archived);
        // titles: width, blank, and look-alikes after neutralizing
        let e = parse(&one("x", &"\u{4e2d}".repeat(21)), "cfg").unwrap_err();
        assert!(
            e.starts_with("cfg:2:") && e.contains("1..40"),
            "42 columns: {e}"
        );
        assert!(parse(&one("x", &"\u{4e2d}".repeat(20)), "cfg").is_ok());
        let e = parse(&one("x", "   "), "cfg").unwrap_err();
        assert!(e.contains("not blank"), "{e}");
        let two = format!("{}{}", one("x", "a\u{202e}b"), one("x", "a<U+202E>b"));
        let e = parse(&two, "cfg").unwrap_err();
        assert!(
            e.starts_with("cfg:6:") && e.contains("used twice") && !e.contains('\u{202e}'),
            "{e}"
        );
        // section titles in panels errors are quoted and neutralized
        let src = format!(
            "{}[panels]\nglobal = [\"section:x\\u202ey\"]\n",
            one("x", "A")
        );
        let e = parse(&src, "cfg").unwrap_err();
        assert!(
            e.contains("unknown section") && !e.contains('\u{202e}'),
            "{e}"
        );
    }
}
