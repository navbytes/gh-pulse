use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// `~/.config/gh-pulse/config.toml`. Never holds credentials: auth stays with `gh`.
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
}

/// Where gh-pulse opens: the repo of the current directory, or the cross-repo home.
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

/// `[api]`: how gently gh-pulse talks to GitHub.
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

/// Left-column panels, as named in `[panels] show`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
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
}

impl PanelName {
    fn repo_mode(self) -> bool {
        !matches!(
            self,
            PanelName::Review | PanelName::Mine | PanelName::Assigned | PanelName::Involved
        )
    }

    fn global_mode(self) -> bool {
        matches!(
            self,
            PanelName::Review
                | PanelName::Mine
                | PanelName::Assigned
                | PanelName::Involved
                | PanelName::Repos
                | PanelName::Files
        )
    }

    pub fn word(self) -> String {
        format!("{self:?}").to_lowercase()
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

    /// `show` without `files` when `prs` is not shown (Files only ever describes the selected PR).
    fn effective(&self) -> Vec<PanelName> {
        let prs = self.show.contains(&PanelName::Prs);
        self.show
            .iter()
            .copied()
            .filter(|n| prs || *n != PanelName::Files)
            .collect()
    }

    /// The global home's panels (`files` needs a PR section).
    pub fn global_layout(&self) -> Vec<PanelSpec> {
        let td = |id, label, short| TabDef { id, label, short };
        let prs = self
            .global
            .iter()
            .any(|n| matches!(n, PanelName::Review | PanelName::Mine | PanelName::Involved));
        self.global
            .iter()
            .copied()
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

    pub fn layout(&self) -> Vec<PanelSpec> {
        self.effective()
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
    fn check(&self) -> Result<(), (&'static str, &'static str, String)> {
        let lower = |x: &dyn std::fmt::Debug| format!("{x:?}").to_lowercase();
        for n in &self.show {
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
                    format!("panels.global: {} listed twice", n.word()),
                ));
            }
        }
        if self.global_layout().is_empty() {
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
                    format!("panels.show: {} listed twice", lower(n)),
                ));
            }
        }
        if self.effective().is_empty() {
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
    Some(base.join("gh-pulse").join("config.toml"))
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
    if let Err((section, key, msg)) = cfg.panels.check() {
        // the key inside its section; the section header if the key is implied (default tabs)
        let lines: Vec<&str> = src.lines().collect();
        let head = lines.iter().position(|l| l.trim() == section);
        let line = head
            .and_then(|h| {
                lines[h + 1..]
                    .iter()
                    .take_while(|l| !l.trim_start().starts_with('['))
                    .position(|l| l.trim_start().starts_with(key))
                    .map(|k| h + 2 + k)
            })
            .or(head.map(|h| h + 1))
            .unwrap_or(1);
        return Err(format!("{name}:{line}: {msg}"));
    }
    if let Err((key, msg)) = cfg.api.check() {
        let lines: Vec<&str> = src.lines().collect();
        let head = lines.iter().position(|l| l.trim() == "[api]");
        let line = head
            .and_then(|h| {
                lines[h + 1..]
                    .iter()
                    .take_while(|l| !l.trim_start().starts_with('['))
                    .position(|l| l.trim_start().starts_with(key))
                    .map(|k| h + 2 + k)
            })
            .or(head.map(|h| h + 1))
            .unwrap_or(1);
        return Err(format!("{name}:{line}: {msg}"));
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
        let dir = std::env::temp_dir().join(format!("gh-pulse-test-{}", std::process::id()));
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
        c.panels.layout().iter().map(|p| p.name).collect()
    }

    #[test]
    fn panels_default_to_the_five_panel_layout() {
        use PanelName::*;
        let c = parse("", "t").unwrap();
        assert_eq!(names(&c), [Prs, Files, Issues, Actions, Repo]);
        let tabs: Vec<_> = c.panels.layout().iter().map(|p| p.tabs.len()).collect();
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
        let l = c.panels.layout();
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
        let dir = std::env::temp_dir().join(format!("gh-pulse-panels-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("gh-pulse-api-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("gh-pulse-ui-{}", std::process::id()));
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
            c.panels.global_layout().iter().map(|p| p.name).collect()
        };
        let c = parse("", "t").unwrap();
        assert_eq!(
            g(&c),
            [Review, Mine, Assigned, Repos],
            "Involved is off by default"
        );
        let tabs: Vec<_> = c
            .panels
            .global_layout()
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
            c.panels.layout().iter().map(|p| p.name).collect::<Vec<_>>(),
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
}
