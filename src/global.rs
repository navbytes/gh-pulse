//! The global home: cross-repo sections (review requested, my PRs, assigned issues, involved) built
//! from GitHub search, narrowed by a scope (all / favorites / one org / one repo). Everything here is
//! pure or a single `gh search` call per section and chunk: the app decides when to call it.
use crate::browse::RepoRow;
use crate::config::{self, MAX_OPS, ReposCfg, SectionCfg, SectionKind};
use crate::gh::{self, Item, Kind};
use crate::state::valid_repo;

/// What the global sections are narrowed to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    Favorites,
    Org(String),
    Repo(String),
}

/// A GitHub login or organization name.
pub fn valid_owner(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 39
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && crate::state::hyphens_ok(s)
}

impl Scope {
    /// The persisted / typed form: `all`, `favorites`, `org:x`, `repo:a/b`. Anything else is None.
    pub fn parse(s: &str) -> Option<Scope> {
        match s {
            "all" => Some(Scope::All),
            "favorites" => Some(Scope::Favorites),
            _ => match s.split_once(':')? {
                ("org", o) if valid_owner(o) => Some(Scope::Org(o.to_string())),
                ("repo", r) if valid_repo(r) => Some(Scope::Repo(r.to_string())),
                _ => None,
            },
        }
    }

    pub fn key(&self) -> String {
        match self {
            Scope::All => "all".into(),
            Scope::Favorites => "favorites".into(),
            Scope::Org(o) => format!("org:{o}"),
            Scope::Repo(r) => format!("repo:{r}"),
        }
    }

    /// For the header: `all`, `favorites`, `org:x`, `repo:x`.
    pub fn label(&self) -> String {
        self.key()
    }
}

/// `(a OR b OR c)` as separate search terms (a bare repeat of `repo:` would AND, and match nothing).
fn or_group(mut terms: Vec<String>) -> Vec<String> {
    if terms.len() < 2 {
        return terms;
    }
    let n = terms.len();
    terms[0].insert(0, '(');
    terms[n - 1].push(')');
    let mut out = Vec::with_capacity(2 * n - 1);
    for (i, t) in terms.into_iter().enumerate() {
        if i > 0 {
            out.push("OR".to_string());
        }
        out.push(t);
    }
    out
}

/// `repo:` qualifiers per search call (GitHub allows at most 5 boolean operators in a query).
pub const FAV_CHUNK: usize = 4;
/// Calls per section refresh for the favorites scope: at most 16 favorites are searched.
pub const MAX_CHUNKS: usize = 4;

/// The qualifier terms of each search call for `scope`, and how many favorites did not fit.
/// Names that are not plain `owner/name` are ignored (a config typo must not become a qualifier).
pub fn scope_chunks(scope: &Scope, favorites: &[String]) -> (Vec<Vec<String>>, usize) {
    scope_chunks_by(scope, favorites, FAV_CHUNK)
}

/// `scope_chunks` with `per` favorites in each group (a custom filter's own operators eat into the budget).
fn scope_chunks_by(scope: &Scope, favorites: &[String], per: usize) -> (Vec<Vec<String>>, usize) {
    match scope {
        Scope::All => (vec![vec![]], 0),
        Scope::Org(o) => (vec![vec![format!("org:{o}")]], 0),
        Scope::Repo(r) => (vec![vec![format!("repo:{r}")]], 0),
        Scope::Favorites => {
            let favs: Vec<&String> = favorites.iter().filter(|f| valid_repo(f)).collect();
            let shown = favs.len().min(per * MAX_CHUNKS);
            let chunks = favs[..shown]
                .chunks(per)
                .map(|c| or_group(c.iter().map(|r| format!("repo:{r}")).collect()))
                .collect();
            (chunks, favs.len() - shown)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Review,
    MyPrs,
    Assigned,
    Involved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Search {
    Prs,
    Issues,
}

impl Search {
    fn cmd(self) -> &'static str {
        match self {
            Search::Prs => "prs",
            Search::Issues => "issues",
        }
    }

    fn kind(self) -> Kind {
        match self {
            Search::Prs => Kind::Pr,
            Search::Issues => Kind::Issue,
        }
    }

    fn fields(self) -> &'static str {
        match self {
            Search::Prs => "number,title,url,state,isDraft,author,labels,body,repository,updatedAt",
            Search::Issues => "number,title,url,state,author,labels,body,repository,updatedAt",
        }
    }
}

/// The qualifiers of a section's tab, one entry per search it needs.
fn base(sec: Section, tab: usize) -> Vec<(Search, Vec<&'static str>)> {
    let open = |who: &'static str| vec![who, "is:open", "archived:false"];
    match (sec, tab) {
        (Section::Review, _) => vec![(Search::Prs, open("review-requested:@me"))],
        (Section::MyPrs, 1) => vec![(
            Search::Prs,
            vec!["author:@me", "is:merged", "archived:false"],
        )],
        (Section::MyPrs, 2) => vec![(
            Search::Prs,
            vec!["author:@me", "is:closed", "is:unmerged", "archived:false"],
        )],
        (Section::MyPrs, _) => vec![(Search::Prs, open("author:@me"))],
        (Section::Assigned, 1) => vec![(Search::Issues, open("author:@me"))],
        (Section::Assigned, 2) => vec![(Search::Issues, open("mentions:@me"))],
        (Section::Assigned, _) => vec![(Search::Issues, open("assignee:@me"))],
        (Section::Involved, _) => vec![
            (Search::Prs, open("involves:@me")),
            (Search::Issues, open("involves:@me")),
        ],
    }
}

/// Results per request (one search page); a second, larger request tops up a list the hidden-repo
/// filter emptied.
pub const PAGE: usize = 100;
const TOP_UP: usize = 200;
/// A list with fewer visible rows than this after filtering is topped up.
const FILL: usize = 30;

type Calls = Vec<(Kind, Vec<String>)>;

/// One `gh search` per search and scope chunk: `terms` follow `--`, so they can never be flags.
fn build_calls(
    base: Vec<(Search, Vec<String>)>,
    scope: &Scope,
    favorites: &[String],
    per: usize,
    limit: usize,
) -> (Calls, usize) {
    let (chunks, not_shown) = scope_chunks_by(scope, favorites, per);
    let mut out = vec![];
    for (kind, terms) in base {
        for chunk in &chunks {
            let mut a: Vec<String> = [
                "search",
                kind.cmd(),
                &format!("--limit={limit}"),
                "--json",
                kind.fields(),
                "--sort=updated",
                "--order=desc",
                "--",
            ]
            .map(String::from)
            .to_vec();
            a.extend(terms.iter().cloned());
            a.extend(chunk.iter().cloned());
            out.push((kind.kind(), a));
        }
    }
    (out, not_shown)
}

/// The exact `gh` argument lists (after `gh`) for a section: one per search and scope chunk, plus
/// the number of favorites that did not fit.
pub fn calls(
    sec: Section,
    tab: usize,
    scope: &Scope,
    favorites: &[String],
    limit: usize,
) -> (Calls, usize) {
    let base = base(sec, tab)
        .into_iter()
        .map(|(k, t)| (k, t.into_iter().map(String::from).collect()))
        .collect();
    build_calls(base, scope, favorites, FAV_CHUNK, limit)
}

/// A `[[sections]]` search, ready to run: the user's terms plus the defaults we add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Custom {
    search: Search,
    terms: Vec<String>,
    /// Boolean operators in the filter; favorites groups must fit under GitHub's limit with them.
    ops: usize,
    /// The scope qualifiers are appended (global home, filter without a scope qualifier of its own).
    scoped: bool,
    limit: usize,
}

impl Custom {
    /// `repo` is the current repo when the section runs in the repo home. Err only for a filter that
    /// `config::parse_filter` refuses (the config loader has already checked, a test config may not).
    pub fn new(cfg: &SectionCfg, repo: Option<&str>) -> Result<Custom, String> {
        let f = config::parse_filter(&cfg.filter)?;
        let mut terms = f.terms;
        // without parentheses GitHub's OR would swallow the qualifiers added below
        // as terms of their own, so no user term's quoting is touched
        if f.has_or {
            terms.insert(0, "(".into());
            terms.push(")".into());
        }
        if !f.has_archived {
            terms.push("archived:false".into());
        }
        if let Some(r) = repo
            && !f.own_repo
        {
            terms.push(format!("repo:{r}"));
        }
        Ok(Custom {
            search: match cfg.kind {
                SectionKind::Prs => Search::Prs,
                SectionKind::Issues => Search::Issues,
            },
            terms,
            ops: f.ops,
            scoped: repo.is_none() && !f.own_scope,
            limit: cfg.limit(),
        })
    }

    fn calls(&self, scope: &Scope, favorites: &[String], limit: usize) -> (Calls, usize) {
        let scope = if self.scoped { scope } else { &Scope::All };
        let per = FAV_CHUNK.min((MAX_OPS + 1).saturating_sub(self.ops)).max(1);
        build_calls(
            vec![(self.search, self.terms.clone())],
            scope,
            favorites,
            per,
            limit,
        )
    }
}

/// What a global panel searches for.
#[derive(Clone, Debug)]
pub enum Query {
    Section(Section, usize),
    Custom(Custom),
}

impl Query {
    pub fn uses_scope(&self) -> bool {
        match self {
            Query::Section(..) => true,
            Query::Custom(c) => c.scoped,
        }
    }

    /// Searches one refresh needs (before any top-up).
    pub fn searches_needed(&self, scope: &Scope, favorites: &[String]) -> usize {
        match self {
            Query::Section(sec, tab) => searches_needed(*sec, *tab, scope, favorites),
            Query::Custom(c) => c.calls(scope, favorites, PAGE).0.len(),
        }
    }

    pub fn fetch(
        &self,
        scope: &Scope,
        favorites: &[String],
        hidden: &ReposCfg,
        quota: Option<Quota>,
    ) -> Result<GlobalList, String> {
        match self {
            Query::Section(sec, tab) => fetch_section(*sec, *tab, scope, favorites, hidden, quota),
            Query::Custom(c) => {
                let page = c.limit;
                let mut l = fetch_with(
                    |n| c.calls(scope, favorites, n),
                    (page, (2 * page).min(TOP_UP)),
                    c.calls(scope, favorites, page).0.len() <= 1,
                    hidden,
                    quota,
                )?;
                l.items.truncate(page);
                Ok(l)
            }
        }
    }
}

pub struct GlobalList {
    pub items: Vec<Item>,
    /// Rows dropped because their repo is hidden.
    pub hidden: usize,
    /// Favorites that did not fit in the search calls.
    pub not_shown: usize,
    /// Some searches failed but others answered.
    pub note: Option<String>,
}

/// Newest update first; one row per item.
pub fn merge(lists: Vec<Vec<Item>>) -> Vec<Item> {
    let mut seen = std::collections::HashSet::new();
    let mut all: Vec<Item> = lists
        .into_iter()
        .flatten()
        .filter(|i| seen.insert(i.key()))
        .collect();
    all.sort_by(|a, b| b.updated.cmp(&a.updated));
    all
}

/// Items outside hidden repos, and how many were dropped.
pub fn without_hidden(items: Vec<Item>, cfg: &ReposCfg) -> (Vec<Item>, usize) {
    let n = items.len();
    let kept: Vec<Item> = items
        .into_iter()
        .filter(|i| !cfg.is_hidden(&i.repo))
        .collect();
    let dropped = n - kept.len();
    (kept, dropped)
}

/// What is left of the search API's budget (30 a minute), when known.
#[derive(Clone, Copy, Debug)]
pub struct Quota {
    pub remaining: u32,
    pub limit: u32,
}

impl Quota {
    /// A bigger second request is only worth it with plenty left: at least 10 and a third of the window.
    fn comfortable(q: Option<Quota>) -> bool {
        q.is_none_or(|q| q.remaining >= 10 && u64::from(q.remaining) * 3 >= u64::from(q.limit))
    }
}

/// Searches a section needs for one refresh (before any top-up).
pub fn searches_needed(sec: Section, tab: usize, scope: &Scope, favorites: &[String]) -> usize {
    calls(sec, tab, scope, favorites, PAGE).0.len()
}

/// One section's list: a page per search (per favorites chunk), hidden repos removed. When that
/// empties a full page, one bigger request re-asks only the searches that came back full, and only
/// for a single-chunk scope with quota to spare (a section never costs more than ~10 searches). A
/// failed search shows what the others found plus a note; only all failing is an error.
pub fn fetch_section(
    sec: Section,
    tab: usize,
    scope: &Scope,
    favorites: &[String],
    hidden: &ReposCfg,
    quota: Option<Quota>,
) -> Result<GlobalList, String> {
    fetch_with(
        |n| calls(sec, tab, scope, favorites, n),
        (PAGE, TOP_UP),
        scope_chunks(scope, favorites).0.len() <= 1,
        hidden,
        quota,
    )
}

/// `calls_for(limit)` builds the searches; `(page, top_up)` are the two request sizes.
fn fetch_with(
    calls_for: impl Fn(usize) -> (Calls, usize),
    (page, top_up): (usize, usize),
    single_chunk: bool,
    hidden: &ReposCfg,
    quota: Option<Quota>,
) -> Result<GlobalList, String> {
    let (calls_, not_shown) = calls_for(page);
    let mut lists: Vec<Option<Vec<Item>>> = vec![];
    let (mut failed, mut first_err) = (0, None);
    for (kind, args) in &calls_ {
        match search(args, *kind, page) {
            Ok(items) => lists.push(Some(items)),
            Err(e) => {
                failed += 1;
                first_err.get_or_insert(e);
                lists.push(None);
            }
        }
    }
    if failed > 0 && failed == calls_.len() {
        return Err(first_err.unwrap_or_default());
    }
    let merged = |lists: &[Option<Vec<Item>>]| merge(lists.iter().flatten().cloned().collect());
    let (mut kept, mut dropped) = without_hidden(merged(&lists), hidden);
    if dropped > 0 && kept.len() < FILL.min(page) && single_chunk && Quota::comfortable(quota) {
        let (again, _) = calls_for(top_up);
        for (i, (kind, args)) in again.iter().enumerate() {
            if lists[i].as_ref().is_some_and(|l| l.len() >= page)
                && let Ok(items) = search(args, *kind, top_up)
            {
                lists[i] = Some(items);
            }
        }
        (kept, dropped) = without_hidden(merged(&lists), hidden);
    }
    let note = (failed > 0).then(|| {
        format!(
            "{failed} of {} searches failed ({}); showing the rest",
            calls_.len(),
            first_err.unwrap_or_default()
        )
    });
    Ok(GlobalList {
        items: kept,
        hidden: dropped,
        not_shown,
        note,
    })
}

/// One `gh search` call, noted against the search API's budget (`limit` rows = one request per 100).
fn search(args: &[String], kind: Kind, limit: usize) -> Result<Vec<Item>, String> {
    let out = gh::gh(args);
    crate::rate::note_searches(limit.div_ceil(100) as u32);
    gh::parse_items(&out?, kind, "")
}

/// Your organizations (`gh api user/orgs`), cached for an hour.
pub fn orgs() -> Result<Vec<String>, String> {
    let out = gh::gh_cached(
        ["api", "user/orgs?per_page=100", "--jq", ".[].login"]
            .map(String::from)
            .to_vec(),
        "1h",
        false,
    )?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|o| valid_owner(o))
        .map(String::from)
        .collect())
}

/// The Repos panel's rows: favorites (config order) or recent repos, with what the cached repo list
/// knows about them (no API call).
pub fn repo_items(
    names: &[String],
    known: &[RepoRow],
    favorites: &ReposCfg,
    now: i64,
    host: &str,
) -> Vec<Item> {
    names
        .iter()
        .filter(|n| valid_repo(n))
        .map(|name| {
            let row = known.iter().find(|r| r.name.eq_ignore_ascii_case(name));
            let fav = favorites.is_fav(name);
            let mut facts = vec![];
            let mut card = vec![name.clone(), String::new()];
            if let Some(r) = row {
                facts.push(if r.private { "private" } else { "public" }.to_string());
                if !r.lang.is_empty() {
                    facts.push(r.lang.clone());
                }
                if !r.pushed.is_empty() {
                    facts.push(crate::browse::ago(&r.pushed, now));
                }
                card.push(format!(
                    "visibility    {}",
                    if r.private { "private" } else { "public" }
                ));
                if !r.lang.is_empty() {
                    card.push(format!("language      {}", r.lang));
                }
                card.push(format!("stars         {}", r.stars));
                card.push(format!("open PRs      {}", r.prs));
                card.push(format!("open issues   {}", r.issues));
                card.push(format!(
                    "last push     {}",
                    crate::browse::ago(&r.pushed, now)
                ));
            } else {
                card.push("(not in your cached repo list yet: open the repo browser, B)".into());
            }
            card.push(String::new());
            card.push(format!(
                "favorite      {}",
                if fav {
                    "yes (f toggles)"
                } else {
                    "no (f adds)"
                }
            ));
            card.push("Enter  open this repo   s  scope the global view to it   H  hide".into());
            Item {
                title: name.clone(),
                repo: name.clone(),
                kind: Kind::Repo,
                state: if fav { "fav".into() } else { String::new() },
                meta: facts.join(" \u{b7} "),
                body: card.join("\n"),
                url: format!("https://{host}/{name}"),
                ..Default::default()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn favs(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("o/r{i}")).collect()
    }

    #[test]
    fn scopes_round_trip_and_reject_anything_that_could_inject_a_qualifier() {
        for s in [
            "all",
            "favorites",
            "org:cli",
            "repo:cli/cli",
            "org:my-org",
            "org:a-b-c",
            "repo:my-org/a--b",
        ] {
            assert_eq!(Scope::parse(s).unwrap().key(), s);
        }
        for bad in [
            "",
            "org:",
            "org:a b",
            "org:a/b",
            "repo:cli",
            "repo:a/b c",
            "repo:a/b label:x",
            "team:x",
            "all:",
            "ORG:cli",
            "org:-o",
            "org:--org",
            "org:x-",
            "org:a--b",
            "repo:-o/r",
            "repo:x-/r",
            "repo:a--b/r",
        ] {
            assert!(Scope::parse(bad).is_none(), "{bad:?} must not parse");
        }
        assert_eq!(Scope::Org("x".into()).label(), "org:x");
    }

    #[test]
    fn favorites_become_chunks_of_four_capped_at_sixteen() {
        let (c, left) = scope_chunks(&Scope::Favorites, &favs(10));
        // each chunk is a parenthesized OR group: 4 + 3, 4 + 3 and 2 + 1 terms
        assert_eq!(c.iter().map(Vec::len).collect::<Vec<_>>(), [7, 7, 3]);
        assert_eq!(left, 0);
        assert_eq!(c[2], ["(repo:o/r8", "OR", "repo:o/r9)"]);
        assert_eq!(c[0][..3], ["(repo:o/r0", "OR", "repo:o/r1"]);
        assert_eq!(c[0][6], "repo:o/r3)");
        let (c, left) = scope_chunks(&Scope::Favorites, &favs(21));
        assert_eq!(
            (c.len(), left),
            (4, 5),
            "16 searched, 5 reported as not shown"
        );
        assert!(c.iter().all(|x| x.len() < 2 * FAV_CHUNK));
        // config junk never reaches a query
        let junk = vec![
            "o/ok".to_string(),
            "a b/c".into(),
            "o/r label:bug".into(),
            "nope".into(),
            "-o/r".into(),
            "x-/r".into(),
            "a--b/r".into(),
        ];
        let (c, left) = scope_chunks(&Scope::Favorites, &junk);
        assert_eq!(
            (c, left),
            (vec![vec!["repo:o/ok".to_string()]], 0),
            "one repo needs no group"
        );
        assert_eq!(scope_chunks(&Scope::Favorites, &[]).0.len(), 0);
        assert_eq!(scope_chunks(&Scope::All, &[]), (vec![vec![]], 0));
        assert_eq!(
            scope_chunks(&Scope::Org("cli".into()), &[]).0,
            [["org:cli"]]
        );
        assert_eq!(
            scope_chunks(&Scope::Repo("a/b".into()), &[]).0,
            [["repo:a/b"]]
        );
    }

    #[test]
    fn section_searches_have_exact_arguments() {
        let one = |sec, tab, scope: &Scope, favs: &[String]| {
            let (c, n) = calls(sec, tab, scope, favs, 100);
            (
                c.into_iter()
                    .map(|(k, a)| (k, a.join(" ")))
                    .collect::<Vec<_>>(),
                n,
            )
        };
        let (c, _) = one(Section::Review, 0, &Scope::All, &[]);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, Kind::Pr);
        assert_eq!(
            c[0].1,
            "search prs --limit=100 --json number,title,url,state,isDraft,author,labels,body,repository,updatedAt --sort=updated --order=desc -- review-requested:@me is:open archived:false"
        );
        let (c, _) = one(Section::MyPrs, 1, &Scope::Org("cli".into()), &[]);
        assert!(
            c[0].1
                .ends_with("-- author:@me is:merged archived:false org:cli"),
            "{}",
            c[0].1
        );
        let (c, _) = one(Section::MyPrs, 2, &Scope::Repo("cli/cli".into()), &[]);
        assert!(
            c[0].1
                .ends_with("-- author:@me is:closed is:unmerged archived:false repo:cli/cli")
        );
        let (c, _) = one(Section::Assigned, 0, &Scope::All, &[]);
        assert_eq!(
            (c[0].0, c[0].1.starts_with("search issues ")),
            (Kind::Issue, true)
        );
        assert!(
            c[0].1
                .contains("--json number,title,url,state,author,labels,body,repository,updatedAt")
        );
        assert!(c[0].1.ends_with("-- assignee:@me is:open archived:false"));
        let (c, _) = one(Section::Assigned, 2, &Scope::All, &[]);
        assert!(c[0].1.ends_with("-- mentions:@me is:open archived:false"));
        // involved: PRs and issues, each per favorites chunk
        let (c, n) = one(Section::Involved, 0, &Scope::Favorites, &favs(6));
        assert_eq!(c.len(), 4, "2 searches x 2 chunks");
        assert_eq!(n, 0);
        assert!(c[0].1.starts_with("search prs ") && c[2].1.starts_with("search issues "));
        assert!(c[0].1.ends_with(
            "-- involves:@me is:open archived:false (repo:o/r0 OR repo:o/r1 OR repo:o/r2 OR repo:o/r3)"
        ));
        assert!(c[1].1.ends_with("(repo:o/r4 OR repo:o/r5)"));
        // values only ever follow `--`, so a repo or org name can't become a flag
        for (_, a) in &c {
            let dd = a.find(" -- ").unwrap();
            assert!(!a[dd + 4..].contains(" --"), "{a}");
        }
    }

    fn item(repo: &str, n: u64, updated: &str) -> Item {
        Item {
            number: n,
            repo: repo.into(),
            updated: updated.into(),
            url: format!("https://github.com/{repo}/pull/{n}"),
            kind: Kind::Pr,
            ..Default::default()
        }
    }

    #[test]
    fn merged_chunks_are_deduplicated_and_sorted_by_update() {
        let a = vec![item("o/a", 1, "2026-01-02"), item("o/a", 2, "2026-01-05")];
        let b = vec![item("o/b", 3, "2026-01-04"), item("o/a", 1, "2026-01-02")];
        let m = merge(vec![a, b]);
        assert_eq!(m.iter().map(|i| i.number).collect::<Vec<_>>(), [2, 3, 1]);
    }

    #[test]
    fn hidden_repos_are_dropped_and_counted() {
        let cfg = ReposCfg {
            hidden: vec!["O/Noisy".into()],
            ..Default::default()
        };
        let items = vec![
            item("o/noisy", 1, ""),
            item("o/ok", 2, ""),
            item("o/noisy", 3, ""),
        ];
        let (kept, dropped) = without_hidden(items, &cfg);
        assert_eq!((kept.len(), dropped), (1, 2));
        assert_eq!(kept[0].repo, "o/ok");
    }

    #[test]
    fn repo_rows_use_only_what_the_cached_list_knows() {
        let known = vec![RepoRow {
            name: "o/known".into(),
            private: true,
            lang: "Go".into(),
            stars: 7,
            pushed: "2026-01-01T00:00:00Z".into(),
            ..Default::default()
        }];
        let cfg = ReposCfg {
            favorites: vec!["o/known".into()],
            ..Default::default()
        };
        let names = vec!["o/known".to_string(), "o/unknown".into(), "bad name".into()];
        let rows = repo_items(&names, &known, &cfg, 1_800_000_000, "ghe.example.com");
        assert_eq!(
            rows[0].url, "https://ghe.example.com/o/known",
            "the configured host, not github.com"
        );
        assert_eq!(rows.len(), 2, "invalid names are skipped");
        assert_eq!((rows[0].state.as_str(), rows[0].kind), ("fav", Kind::Repo));
        assert!(
            rows[0].meta.starts_with("private \u{b7} Go"),
            "{}",
            rows[0].meta
        );
        assert!(rows[0].body.contains("stars         7"));
        assert!(rows[1].meta.is_empty() && rows[1].body.contains("not in your cached repo list"));
        assert_eq!(rows[1].state, "");
    }

    #[cfg(unix)]
    fn search_json(repos: &[(&str, u64)]) -> String {
        let rows: Vec<String> = repos
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

    #[cfg(unix)]
    #[test]
    fn a_section_runs_one_search_per_scope_chunk_with_exact_arguments() {
        let shim = crate::testshim::Shim::new();
        shim.set("searchprs.out", &search_json(&[("a/b", 1), ("c/d", 2)]));
        let l = fetch_section(
            Section::Review,
            0,
            &Scope::Favorites,
            &favs(6),
            &ReposCfg::default(),
            None,
        )
        .unwrap();
        let calls = shim.calls();
        assert_eq!(calls.len(), 2, "6 favorites: chunks of 4 and 2: {calls:?}");
        let head = "search prs --limit=100 --json number,title,url,state,isDraft,author,labels,body,repository,updatedAt --sort=updated --order=desc -- review-requested:@me is:open archived:false";
        assert_eq!(
            calls[0],
            format!("{head} (repo:o/r0 OR repo:o/r1 OR repo:o/r2 OR repo:o/r3)")
        );
        assert_eq!(calls[1], format!("{head} (repo:o/r4 OR repo:o/r5)"));
        // the same two rows came back for both chunks: merged to one list, newest first
        assert_eq!(l.items.len(), 2);
        assert_eq!((l.hidden, l.not_shown), (0, 0));
        assert!(l.items[0].updated >= l.items[1].updated);
        assert_eq!(l.items[0].repo.split('/').count(), 2);
        // 21 favorites: 16 searched in 4 calls, 5 reported as left out
        shim.clear("calls.log");
        let l = fetch_section(
            Section::Review,
            0,
            &Scope::Favorites,
            &favs(21),
            &ReposCfg::default(),
            None,
        )
        .unwrap();
        assert_eq!((shim.calls().len(), l.not_shown), (4, 5));
        // no favorites: no search at all
        shim.clear("calls.log");
        let l = fetch_section(
            Section::Review,
            0,
            &Scope::Favorites,
            &[],
            &ReposCfg::default(),
            None,
        )
        .unwrap();
        assert!(shim.calls().is_empty() && l.items.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_list_the_hidden_filter_emptied_is_topped_up_once() {
        let shim = crate::testshim::Shim::new();
        let mut rows: Vec<(&str, u64)> = (0..95).map(|i| ("noisy/bot", i)).collect();
        rows.extend((0..5).map(|i| ("keep/me", 1000 + i)));
        shim.set("searchprs.out", &search_json(&rows));
        let cfg = ReposCfg {
            hidden: vec!["noisy/bot".into()],
            ..Default::default()
        };
        let l = fetch_section(Section::MyPrs, 0, &Scope::All, &[], &cfg, None).unwrap();
        let calls = shim.calls();
        assert_eq!(
            calls.len(),
            2,
            "a full page that mostly vanished is asked for again: {calls:?}"
        );
        assert!(calls[0].contains("--limit=100") && calls[1].contains("--limit=200"));
        assert!(calls[1].ends_with("-- author:@me is:open archived:false"));
        assert_eq!((l.items.len(), l.hidden), (5, 95));
        // enough visible rows, or a page that wasn't full: one search
        shim.clear("calls.log");
        let l = fetch_section(
            Section::MyPrs,
            0,
            &Scope::All,
            &[],
            &ReposCfg::default(),
            None,
        )
        .unwrap();
        assert_eq!((shim.calls().len(), l.hidden), (1, 0));
        shim.clear("calls.log");
        shim.set(
            "searchprs.out",
            &search_json(&[("noisy/bot", 1), ("keep/me", 2)]),
        );
        let l = fetch_section(Section::MyPrs, 0, &Scope::All, &[], &cfg, None).unwrap();
        assert_eq!((shim.calls().len(), l.items.len(), l.hidden), (1, 1, 1));
    }

    #[cfg(unix)]
    #[test]
    fn involved_asks_for_prs_and_issues_and_a_failure_is_an_error_not_a_blank() {
        let shim = crate::testshim::Shim::new();
        shim.set("searchprs.out", &search_json(&[("a/b", 1)]));
        shim.set("searchissues.out", "[]");
        let l = fetch_section(
            Section::Involved,
            0,
            &Scope::Org("cli".into()),
            &[],
            &ReposCfg::default(),
            None,
        )
        .unwrap();
        let c = shim.calls();
        assert_eq!(c.len(), 2);
        assert!(c[0].starts_with("search prs") && c[1].starts_with("search issues"));
        assert!(c[1].ends_with("-- involves:@me is:open archived:false org:cli"));
        assert_eq!(l.items[0].kind, Kind::Pr);
        // one of the two searches failing still shows the other's rows, with a note
        shim.set("searchprs.err", "HTTP 422: Validation Failed");
        let l = fetch_section(
            Section::Involved,
            0,
            &Scope::All,
            &[],
            &ReposCfg::default(),
            None,
        )
        .unwrap();
        assert!(
            l.items.is_empty()
                && l.note
                    .as_deref()
                    .unwrap()
                    .contains("1 of 2 searches failed")
        );
        shim.set("searchprs.out", &search_json(&[("a/b", 1)]));
        shim.clear("searchprs.err");
        shim.set("searchissues.err", "HTTP 500");
        let l = fetch_section(
            Section::Involved,
            0,
            &Scope::All,
            &[],
            &ReposCfg::default(),
            None,
        )
        .unwrap();
        assert_eq!((l.items.len(), l.note.is_some()), (1, true));
        // every search failing is an error, not a blank
        shim.set("searchprs.err", "HTTP 422: Validation Failed");
        assert!(
            fetch_section(
                Section::Involved,
                0,
                &Scope::All,
                &[],
                &ReposCfg::default(),
                None
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_top_up_is_cheap_conditional_and_bounded() {
        let shim = crate::testshim::Shim::new();
        let mut rows: Vec<(&str, u64)> = (0..100).map(|i| ("noisy/bot", i)).collect();
        rows.truncate(100);
        shim.set("searchprs.out", &search_json(&rows));
        shim.set("searchissues.out", &search_json(&[("keep/me", 1)]));
        let cfg = ReposCfg {
            hidden: vec!["noisy/bot".into()],
            ..Default::default()
        };
        // Involved, one chunk: only the search that came back full is asked again
        let l = fetch_section(Section::Involved, 0, &Scope::All, &[], &cfg, None).unwrap();
        let c = shim.calls();
        assert_eq!(c.len(), 3, "{c:?}");
        assert!(c[2].starts_with("search prs") && c[2].contains("--limit=200"));
        assert_eq!((l.items.len(), l.hidden), (1, 100));
        // several favorites chunks: no top-up at all, and never more than 2 x 4 searches
        shim.clear("calls.log");
        let favs: Vec<String> = (0..16).map(|i| format!("o/r{i}")).collect();
        fetch_section(Section::Involved, 0, &Scope::Favorites, &favs, &cfg, None).unwrap();
        let c = shim.calls();
        assert_eq!(c.len(), 8, "{c:?}");
        assert!(c.iter().all(|x| x.contains("--limit=100")));
        // little search quota left: no top-up either
        shim.clear("calls.log");
        let q = |remaining| {
            Some(Quota {
                remaining,
                limit: 30,
            })
        };
        fetch_section(Section::Involved, 0, &Scope::All, &[], &cfg, q(9)).unwrap();
        assert_eq!(shim.calls().len(), 2, "9 left is not comfortable");
        shim.clear("calls.log");
        fetch_section(Section::Involved, 0, &Scope::All, &[], &cfg, q(10)).unwrap();
        assert_eq!(shim.calls().len(), 3);
        assert!(
            !Quota::comfortable(Some(Quota {
                remaining: 10,
                limit: 60
            })),
            "under a third of the window"
        );
        assert!(Quota::comfortable(None));
        assert_eq!(
            searches_needed(Section::Involved, 0, &Scope::Favorites, &favs),
            8
        );
        assert_eq!(searches_needed(Section::Review, 0, &Scope::All, &[]), 1);
    }

    #[cfg(unix)]
    #[test]
    fn one_failed_favorites_chunk_keeps_the_others_rows() {
        let shim = crate::testshim::Shim::new();
        shim.set("searchprs.out", &search_json(&[("a/b", 1)]));
        // fail only the second chunk: the shim fails calls whose arguments mention r4
        let favs: Vec<String> = (0..6).map(|i| format!("o/r{i}")).collect();
        std::fs::write(
            shim.dir.join("gh"),
            std::fs::read_to_string(shim.dir.join("gh"))
                .unwrap()
                .replace(
                    "case \"$*\" in",
                    "case \"$*\" in\n  *\"repo:o/r4\"*) echo boom >&2; exit 1;;",
                ),
        )
        .unwrap();
        let l = fetch_section(
            Section::Review,
            0,
            &Scope::Favorites,
            &favs,
            &ReposCfg::default(),
            None,
        )
        .unwrap();
        assert_eq!(l.items.len(), 1, "the first chunk's rows survive");
        assert!(
            l.note
                .as_deref()
                .unwrap()
                .contains("1 of 2 searches failed (boom)"),
            "{:?}",
            l.note
        );
    }
    fn sec(kind: SectionKind, filter: &str) -> SectionCfg {
        SectionCfg {
            title: "T".into(),
            kind,
            filter: filter.into(),
            limit: None,
            at: config::Where::Global,
        }
    }

    /// The terms after `--` of the single search a custom section runs.
    fn tail(filter: &str, scope: &Scope, repo: Option<&str>) -> String {
        let c = Custom::new(&sec(SectionKind::Prs, filter), repo).unwrap();
        let (calls, _) = c.calls(scope, &[], 30);
        assert_eq!(calls.len(), 1);
        let a = &calls[0].1;
        let dd = a.iter().position(|x| x == "--").unwrap();
        assert_eq!(
            a[..dd].join(" "),
            "search prs --limit=30 --json number,title,url,state,isDraft,author,labels,body,repository,updatedAt --sort=updated --order=desc"
        );
        assert_eq!(a.iter().filter(|x| *x == "--").count(), 1);
        a[dd + 1..].join(" ")
    }

    #[test]
    fn a_filter_becomes_terms_with_scope_archived_and_repo_added_as_documented() {
        let org = Scope::Org("acme".into());
        let rv = "review-requested:@me";
        let all = Scope::All;
        assert_eq!(
            tail(&format!("is:open {rv}"), &all, None),
            format!("is:open {rv} archived:false")
        );
        assert_eq!(
            tail(&format!("is:open {rv}"), &org, None),
            format!("is:open {rv} archived:false org:acme"),
            "scope qualifiers appended"
        );
        assert_eq!(
            tail("is:open org:other", &org, None),
            "is:open org:other archived:false",
            "a qualifier of its own turns the scope off"
        );
        for own in ["repo:a/b", "user:me", "(repo:a/b OR repo:c/d)"] {
            assert!(
                !tail(&format!("is:open {own}"), &org, None).contains("org:acme"),
                "{own}"
            );
        }
        assert_eq!(
            tail("is:open archived:true", &all, None),
            "is:open archived:true"
        );
        assert_eq!(tail("-archived:true x", &all, None), "-archived:true x");
        assert_eq!(
            tail("is:open", &org, Some("o/r")),
            "is:open archived:false repo:o/r",
            "repo mode: the current repo, not the scope"
        );
        assert_eq!(
            tail("is:open repo:x/y", &org, Some("o/r")),
            "is:open repo:x/y archived:false"
        );
        // OR is grouped so the added qualifiers apply to all of it
        assert_eq!(
            tail("a OR b", &org, None),
            "( a OR b ) archived:false org:acme"
        );
        assert_eq!(
            tail("label:\"good first issue\" -label:wip", &all, None),
            "label:good first issue -label:wip archived:false"
        );
    }

    #[test]
    fn hostile_filters_are_refused_or_stay_data_after_the_separator() {
        for bad in [
            "--web",
            "x --jq=.",
            "a\nb",
            "a\rb",
            "$(touch x)",
            "`id`",
            "a;b",
            "x | y",
            "\"unclosed",
            "",
        ] {
            assert!(
                Custom::new(&sec(SectionKind::Prs, bad), None).is_err(),
                "{bad:?} must be refused"
            );
        }
        // an OR with a user: qualifier is just terms: nothing before `--` changes
        assert_eq!(
            tail("x OR user:y", &Scope::Org("acme".into()), None),
            "( x OR user:y ) archived:false"
        );
        // a quoted phrase cannot smuggle a flag either
        assert!(Custom::new(&sec(SectionKind::Prs, "\"--web\""), None).is_err());
        // terms that merely contain dashes are fine
        assert_eq!(
            tail("fix-it -label:a-b", &Scope::All, None),
            "fix-it -label:a-b archived:false"
        );
    }

    #[test]
    fn favorites_groups_leave_room_for_the_filters_own_operators() {
        let c = |f: &str| Custom::new(&sec(SectionKind::Issues, f), None).unwrap();
        let f6 = favs(6);
        let (calls, left) = c("a OR b").calls(&Scope::Favorites, &f6, 30);
        assert_eq!((calls.len(), left), (2, 0));
        // 5 operators already: single repos, no OR group, at most 4 searches
        let (calls, left) = c("a OR b OR c OR d OR e OR f").calls(&Scope::Favorites, &f6, 30);
        assert_eq!((calls.len(), left), (4, 2));
        assert!(calls[0].1.last().is_some_and(|t| t == "repo:o/r0") && calls[0].0 == Kind::Issue);
        // an own-scope filter ignores favorites entirely: one search
        let (calls, _) = c("org:x").calls(&Scope::Favorites, &f6, 30);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            Query::Custom(c("org:x")).searches_needed(&Scope::Favorites, &f6),
            1
        );
        assert!(!Query::Custom(c("org:x")).uses_scope() && Query::Custom(c("x")).uses_scope());
    }

    #[cfg(unix)]
    #[test]
    fn a_custom_section_is_one_search_limited_filtered_and_topped_up_like_the_others() {
        let shim = crate::testshim::Shim::new();
        let mut cfg = sec(SectionKind::Prs, "is:open author:@me");
        cfg.limit = Some(3);
        let q = Query::Custom(Custom::new(&cfg, None).unwrap());
        let rows: Vec<(&str, u64)> = (1..=5).map(|i| ("keep/me", i)).collect();
        shim.set("searchprs.out", &search_json(&rows));
        let l = q
            .fetch(&Scope::Org("acme".into()), &[], &ReposCfg::default(), None)
            .unwrap();
        let c = shim.calls();
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(
            c[0].contains("--limit=3 ")
                && c[0].ends_with("-- is:open author:@me archived:false org:acme"),
            "{c:?}"
        );
        assert_eq!(l.items.len(), 3, "never more rows than the limit");
        // hidden repos are dropped client side; a full page that emptied is asked for once more, twice as big
        shim.clear("calls.log");
        let hidden = ReposCfg {
            hidden: vec!["noisy/bot".into()],
            ..Default::default()
        };
        shim.set(
            "searchprs.out",
            &search_json(&[("noisy/bot", 1), ("noisy/bot", 2), ("noisy/bot", 3)]),
        );
        let l = q.fetch(&Scope::All, &[], &hidden, None).unwrap();
        let c = shim.calls();
        assert_eq!(c.len(), 2, "{c:?}");
        assert!(c[1].contains("--limit=6 "));
        assert_eq!((l.items.len(), l.hidden), (0, 3));
        // too little search quota left: the page is enough
        shim.clear("calls.log");
        let low = Some(Quota {
            remaining: 9,
            limit: 30,
        });
        q.fetch(&Scope::All, &[], &hidden, low).unwrap();
        assert_eq!(shim.calls().len(), 1);
        // a failure is an error, not a blank list
        shim.set("searchprs.err", "HTTP 422: Validation Failed");
        assert!(q.fetch(&Scope::All, &[], &hidden, None).is_err());
        // every search is counted against the budget
        shim.clear("searchprs.err");
        crate::rate::apply_poll(
            r#"{"resources":{"graphql":{"limit":5000,"remaining":5000,"reset":4102444800},"core":{"limit":5000,"remaining":5000,"reset":4102444800},"search":{"limit":30,"remaining":20,"reset":4102444800}}}"#,
        );
        q.fetch(&Scope::All, &[], &ReposCfg::default(), None)
            .unwrap();
        assert_eq!(
            crate::rate::snapshot().search.map(|b| b.remaining),
            Some(19)
        );
        assert!(
            crate::gh::cmd_log()
                .iter()
                .any(|l| l.contains("rate: search 19/30 left"))
        );
    }
    #[test]
    fn or_groups_never_touch_a_users_term_or_its_quoting() {
        let t = |f: &str| tail(f, &Scope::Org("acme".into()), None);
        let a = "archived:false org:acme";
        assert_eq!(
            t(r#"x OR label:"good first""#),
            format!("( x OR label:good first ) {a}")
        );
        assert_eq!(
            t(r#"label:"good first" OR x"#),
            format!("( label:good first OR x ) {a}")
        );
        assert_eq!(t(r#""a b" OR c"#), format!("( a b OR c ) {a}"));
        assert_eq!(
            t(r#"(label:"a b" OR c) d"#),
            format!("( ( label:a b OR c ) d ) {a}")
        );
        assert_eq!(
            t("((a OR b)(c OR d))"),
            format!("( ( ( a OR b ) ( c OR d ) ) ) {a}")
        );
        assert_eq!(
            t(r#"fix "(not a group)" OR y"#),
            format!("( fix (not a group) OR y ) {a}")
        );
        // parentheses are separate arguments, so no argument ends in `)` or starts with `(` next to a value
        let c = Custom::new(
            &sec(SectionKind::Issues, r#"x OR label:"good first""#),
            None,
        )
        .unwrap();
        let (calls, _) = c.calls(&Scope::All, &[], 30);
        let dd = calls[0].1.iter().position(|x| x == "--").unwrap();
        assert_eq!(
            calls[0].1[dd + 1..],
            ["(", "x", "OR", "label:good first", ")", "archived:false"]
        );
    }
}
