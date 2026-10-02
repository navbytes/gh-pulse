use crate::config::ReposCfg;
use crate::gh;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

/// Safety valve for accounts with a huge number of repos; the UI shows "N+" past it.
pub const MAX_REPOS: usize = 3000;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RepoRow {
    pub name: String,
    pub owner: String,
    pub owner_org: bool,
    pub private: bool,
    pub fork: bool,
    pub archived: bool,
    pub lang: String,
    pub stars: u32,
    /// ISO timestamp; sorts lexicographically.
    pub pushed: String,
    pub prs: u32,
    pub issues: u32,
}

pub struct Page {
    pub viewer: String,
    pub rows: Vec<RepoRow>,
    pub next: Option<String>,
}

const QUERY: &str = "query($after:String){viewer{login repositories(first:100,after:$after,affiliations:[OWNER,COLLABORATOR,ORGANIZATION_MEMBER],orderBy:{field:PUSHED_AT,direction:DESC}){pageInfo{hasNextPage endCursor} nodes{nameWithOwner isPrivate isFork isArchived stargazerCount pushedAt primaryLanguage{name} owner{__typename login} issues(states:OPEN){totalCount} pullRequests(states:OPEN){totalCount}}}}}";

pub fn parse_page(json: &str) -> Result<Page, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("bad gh output: {e}"))?;
    if let Some(e) = v["errors"][0]["message"].as_str() {
        return Err(e.to_string());
    }
    let repos = &v["data"]["viewer"]["repositories"];
    let s = |n: &Value, p: &str| {
        let t = n.pointer(p).and_then(Value::as_str).unwrap_or("");
        crate::sanitize::clean(t).into_owned()
    };
    let rows = repos["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| RepoRow {
            name: s(n, "/nameWithOwner"),
            owner: s(n, "/owner/login"),
            owner_org: n["owner"]["__typename"] == "Organization",
            private: n["isPrivate"] == true,
            fork: n["isFork"] == true,
            archived: n["isArchived"] == true,
            lang: s(n, "/primaryLanguage/name"),
            stars: n["stargazerCount"].as_u64().unwrap_or(0) as u32,
            pushed: s(n, "/pushedAt"),
            prs: n["pullRequests"]["totalCount"].as_u64().unwrap_or(0) as u32,
            issues: n["issues"]["totalCount"].as_u64().unwrap_or(0) as u32,
        })
        .collect();
    let next = (repos["pageInfo"]["hasNextPage"] == true)
        .then(|| repos["pageInfo"]["endCursor"].as_str().map(str::to_string))
        .flatten();
    Ok(Page {
        viewer: s(&v, "/data/viewer/login"),
        rows,
        next,
    })
}

pub fn fetch_page(after: Option<&str>) -> Result<Page, String> {
    let mut a = vec![
        "api".to_string(),
        "graphql".into(),
        "-f".into(),
        format!("query={QUERY}"),
    ];
    if let Some(c) = after {
        a.extend(["-f".to_string(), format!("after={c}")]);
    }
    parse_page(&gh::gh(a)?)
}

/// "3d", "5mo", "2y" since `iso`, given the current unix time.
pub fn ago(iso: &str, now: i64) -> String {
    let Some(t) = gh::epoch(iso) else {
        return String::new();
    };
    let d = (now - t).max(0);
    match d {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", d / 60),
        3600..=86_399 => format!("{}h", d / 3600),
        86_400..=2_591_999 => format!("{}d", d / 86_400),
        2_592_000..=31_535_999 => format!("{}mo", d / 2_592_000),
        _ => format!("{}y", d / 31_536_000),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sort {
    Pushed,
    Name,
    Stars,
}

impl Sort {
    fn next(self) -> Self {
        match self {
            Sort::Pushed => Sort::Name,
            Sort::Name => Sort::Stars,
            Sort::Stars => Sort::Pushed,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Sort::Pushed => "pushed",
            Sort::Name => "name",
            Sort::Stars => "stars",
        }
    }

    /// Natural direction: newest / most-starred first, names A-Z.
    fn default_desc(self) -> bool {
        self != Sort::Name
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeF {
    All,
    Owned,
    /// Someone else's personal repo you collaborate on.
    Member,
    Org,
    Favorites,
}

impl TypeF {
    fn next(self) -> Self {
        match self {
            TypeF::All => TypeF::Owned,
            TypeF::Owned => TypeF::Member,
            TypeF::Member => TypeF::Org,
            TypeF::Org => TypeF::Favorites,
            TypeF::Favorites => TypeF::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TypeF::All => "all",
            TypeF::Owned => "owned",
            TypeF::Member => "member",
            TypeF::Org => "org",
            TypeF::Favorites => "favorites",
        }
    }
}

pub struct Browser {
    pub rows: Vec<RepoRow>,
    pub viewer: String,
    pub loading: bool,
    pub truncated: bool,
    pub error: Option<String>,
    pub query: String,
    /// The search box has the keyboard (every printable key is text).
    pub typing: bool,
    pub sort: Sort,
    pub desc: bool,
    pub ty: TypeF,
    pub show_hidden: bool,
    pub cursor: usize,
}

pub enum Out {
    None,
    Close,
    Switch(String),
    Fav(String),
    Hide(String),
    Reload,
}

impl Browser {
    pub fn new() -> Self {
        Browser {
            rows: vec![],
            viewer: String::new(),
            loading: true,
            truncated: false,
            error: None,
            query: String::new(),
            typing: true,
            sort: Sort::Pushed,
            desc: true,
            ty: TypeF::All,
            show_hidden: false,
            cursor: 0,
        }
    }

    fn type_ok(&self, r: &RepoRow, cfg: &ReposCfg) -> bool {
        let mine = r.owner.eq_ignore_ascii_case(&self.viewer);
        match self.ty {
            TypeF::All => true,
            TypeF::Owned => mine,
            TypeF::Member => !mine && !r.owner_org,
            TypeF::Org => r.owner_org,
            TypeF::Favorites => cfg.is_fav(&r.name),
        }
    }

    /// Row indices after filtering and sorting; favorites always come first.
    pub fn visible(&self, cfg: &ReposCfg) -> Vec<usize> {
        let q = self.query.to_lowercase();
        let mut v: Vec<usize> = (0..self.rows.len())
            .filter(|&i| {
                let r = &self.rows[i];
                (self.show_hidden || !cfg.is_hidden(&r.name))
                    && self.type_ok(r, cfg)
                    && (q.is_empty()
                        || r.name.to_lowercase().contains(&q)
                        || r.lang.to_lowercase().contains(&q))
            })
            .collect();
        v.sort_by(|&a, &b| {
            let (ra, rb) = (&self.rows[a], &self.rows[b]);
            let by = match self.sort {
                Sort::Pushed => ra.pushed.cmp(&rb.pushed),
                Sort::Name => ra.name.to_lowercase().cmp(&rb.name.to_lowercase()),
                Sort::Stars => ra.stars.cmp(&rb.stars),
            };
            cfg.is_fav(&rb.name)
                .cmp(&cfg.is_fav(&ra.name))
                .then(if self.desc { by.reverse() } else { by })
        });
        v
    }

    pub fn selected(&self, cfg: &ReposCfg) -> Option<&RepoRow> {
        let v = self.visible(cfg);
        v.get(self.cursor.min(v.len().saturating_sub(1)))
            .map(|&i| &self.rows[i])
    }

    pub fn key(&mut self, k: KeyEvent, cfg: &ReposCfg) -> Out {
        let n = self.visible(cfg).len();
        let max = n.saturating_sub(1);
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let mv = |c: usize, d: isize| c.min(max).saturating_add_signed(d).min(max);
        match k.code {
            KeyCode::Down => return self.go(mv(self.cursor, 1)),
            KeyCode::Up => return self.go(mv(self.cursor, -1)),
            KeyCode::PageDown => return self.go(mv(self.cursor, 10)),
            KeyCode::PageUp => return self.go(mv(self.cursor, -10)),
            KeyCode::Home => return self.go(0),
            KeyCode::End => return self.go(max),
            KeyCode::Char('d') if ctrl => return self.go(mv(self.cursor, 10)),
            KeyCode::Char('u') if ctrl => return self.go(mv(self.cursor, -10)),
            _ => {}
        }
        if self.typing {
            match k.code {
                KeyCode::Esc | KeyCode::Enter => self.typing = false,
                KeyCode::Backspace => {
                    self.query.pop();
                    self.cursor = 0;
                }
                KeyCode::Char(c) if !ctrl => {
                    self.query.push(c);
                    self.cursor = 0;
                }
                _ => {}
            }
            return Out::None;
        }
        let name = self.selected(cfg).map(|r| r.name.clone());
        match k.code {
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.cursor = 0;
            }
            KeyCode::Esc | KeyCode::Char('q') => return Out::Close,
            KeyCode::Enter => return name.map_or(Out::None, Out::Switch),
            KeyCode::Char('/') => self.typing = true,
            KeyCode::Char('j') => return self.go(mv(self.cursor, 1)),
            KeyCode::Char('k') => return self.go(mv(self.cursor, -1)),
            KeyCode::Char('g') => return self.go(0),
            KeyCode::Char('G') => return self.go(max),
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.desc = self.sort.default_desc();
            }
            KeyCode::Char('S') => self.desc = !self.desc,
            KeyCode::Char('T') => {
                self.ty = self.ty.next();
                self.cursor = 0;
            }
            KeyCode::Char('.') => self.show_hidden = !self.show_hidden,
            KeyCode::Char('f') => return name.map_or(Out::None, Out::Fav),
            KeyCode::Char('H') => return name.map_or(Out::None, Out::Hide),
            KeyCode::Char('r') => return Out::Reload,
            _ => {}
        }
        Out::None
    }

    fn go(&mut self, c: usize) -> Out {
        self.cursor = c;
        Out::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browser() -> Browser {
        let page = parse_page(include_str!("../tests/repos.json")).unwrap();
        let mut b = Browser::new();
        b.viewer = page.viewer;
        b.rows = page.rows;
        b
    }

    fn names(b: &Browser, cfg: &ReposCfg) -> Vec<String> {
        b.visible(cfg)
            .iter()
            .map(|&i| b.rows[i].name.clone())
            .collect()
    }

    fn press(b: &mut Browser, c: char, cfg: &ReposCfg) -> Out {
        b.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), cfg)
    }

    #[test]
    fn parses_graphql_page() {
        let p = parse_page(include_str!("../tests/repos.json")).unwrap();
        assert_eq!(p.viewer, "octocat");
        assert_eq!(p.rows.len(), 5);
        assert_eq!(p.next.as_deref(), Some("CURSOR1"));
        let r = &p.rows[1];
        assert_eq!(
            (r.name.as_str(), r.owner.as_str(), r.owner_org),
            ("hello-org/web", "hello-org", true)
        );
        assert!(r.private && !r.fork && r.archived);
        assert_eq!(
            (r.lang.as_str(), r.stars, r.prs, r.issues),
            ("TypeScript", 42, 3, 7)
        );
        assert_eq!(p.rows[3].lang, "", "no primary language");
        assert!(parse_page(r#"{"errors":[{"message":"Bad credentials"}]}"#).is_err());
    }

    #[test]
    fn sorts_filters_and_favorites_first() {
        let mut b = browser();
        let mut cfg = ReposCfg::default();
        assert_eq!(
            names(&b, &cfg)[0],
            "octocat/hello-world",
            "newest push first"
        );
        b.sort = Sort::Name;
        b.desc = false;
        assert_eq!(names(&b, &cfg)[0], "friend/shared");
        b.sort = Sort::Stars;
        b.desc = true;
        assert_eq!(names(&b, &cfg)[0], "hello-org/web");
        cfg.toggle_fav("friend/shared");
        assert_eq!(
            names(&b, &cfg)[0],
            "friend/shared",
            "favorites sort to the top"
        );
        b.query = "TYPE".into(); // matches language too, case-insensitively
        assert_eq!(names(&b, &cfg), ["hello-org/web"]);
        b.query.clear();
        b.ty = TypeF::Owned;
        assert!(names(&b, &cfg).iter().all(|n| n.starts_with("octocat/")));
        b.ty = TypeF::Org;
        assert_eq!(names(&b, &cfg), ["hello-org/web", "hello-org/infra"]);
        b.ty = TypeF::Member;
        assert_eq!(names(&b, &cfg), ["friend/shared"]);
        b.ty = TypeF::Favorites;
        assert_eq!(names(&b, &cfg), ["friend/shared"]);
    }

    #[test]
    fn hidden_repos_vanish_until_shown() {
        let b = &mut browser();
        let mut cfg = ReposCfg::default();
        cfg.toggle_hidden("Hello-Org/Web");
        assert_eq!(names(b, &cfg).len(), 4);
        assert!(!names(b, &cfg).contains(&"hello-org/web".to_string()));
        b.show_hidden = true;
        assert_eq!(
            names(b, &cfg).len(),
            5,
            "'.' brings them back (dimmed in the UI)"
        );
    }

    #[test]
    fn keys_drive_state_and_report_intent() {
        let (b, cfg) = (&mut browser(), ReposCfg::default());
        assert!(b.typing, "opens in the search box");
        for c in "hello".chars() {
            press(b, c, &cfg);
        }
        assert_eq!(b.query, "hello");
        b.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &cfg);
        assert!(!b.typing);
        assert!(matches!(press(b, 'f', &cfg), Out::Fav(n) if n == "octocat/hello-world"));
        assert!(matches!(press(b, 'H', &cfg), Out::Hide(_)));
        assert!(matches!(press(b, 's', &cfg), Out::None) && b.sort == Sort::Name && !b.desc);
        press(b, 'S', &cfg);
        assert!(b.desc);
        assert!(matches!(
            b.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &cfg),
            Out::Switch(_)
        ));
        assert!(
            matches!(
                b.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &cfg),
                Out::None
            ),
            "Esc clears the query first"
        );
        assert!(b.query.is_empty());
        assert!(matches!(
            b.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &cfg),
            Out::Close
        ));
    }

    #[test]
    fn relative_time() {
        let now = gh::epoch("2026-10-03T12:00:00Z").unwrap();
        assert_eq!(ago("2026-10-03T11:59:30Z", now), "now");
        assert_eq!(ago("2026-10-03T09:00:00Z", now), "3h");
        assert_eq!(ago("2026-09-30T12:00:00Z", now), "3d");
        assert_eq!(ago("2026-05-01T12:00:00Z", now), "5mo");
        assert_eq!(ago("2024-01-01T00:00:00Z", now), "2y");
        assert_eq!(ago("garbage", now), "");
    }
}
