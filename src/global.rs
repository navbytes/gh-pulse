//! The global home: cross-repo sections (review requested, my PRs, assigned issues, involved) built
//! from GitHub search, narrowed by a scope (all / favorites / one org / one repo). Everything here is
//! pure or a single `gh search` call per section and chunk: the app decides when to call it.
use crate::browse::RepoRow;
use crate::config::ReposCfg;
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
    !s.is_empty() && s.len() <= 39 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
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
    match scope {
        Scope::All => (vec![vec![]], 0),
        Scope::Org(o) => (vec![vec![format!("org:{o}")]], 0),
        Scope::Repo(r) => (vec![vec![format!("repo:{r}")]], 0),
        Scope::Favorites => {
            let favs: Vec<&String> = favorites.iter().filter(|f| valid_repo(f)).collect();
            let shown = favs.len().min(FAV_CHUNK * MAX_CHUNKS);
            let chunks = favs[..shown]
                .chunks(FAV_CHUNK)
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

/// The exact `gh` argument lists (after `gh`) for a section: one per search and scope chunk, plus
/// the number of favorites that did not fit.
pub fn calls(
    sec: Section,
    tab: usize,
    scope: &Scope,
    favorites: &[String],
    limit: usize,
) -> (Vec<(Kind, Vec<String>)>, usize) {
    let (chunks, not_shown) = scope_chunks(scope, favorites);
    let mut out = vec![];
    for (kind, terms) in base(sec, tab) {
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
            a.extend(terms.iter().map(|t| t.to_string()));
            a.extend(chunk.iter().cloned());
            out.push((kind.kind(), a));
        }
    }
    (out, not_shown)
}

pub struct GlobalList {
    pub items: Vec<Item>,
    /// Rows dropped because their repo is hidden.
    pub hidden: usize,
    /// Favorites that did not fit in the search calls.
    pub not_shown: usize,
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

fn run(
    sec: Section,
    tab: usize,
    scope: &Scope,
    favorites: &[String],
    limit: usize,
) -> Result<(Vec<Item>, usize, bool), String> {
    let (calls, not_shown) = calls(sec, tab, scope, favorites, limit);
    let mut lists = vec![];
    let mut full = false;
    for (kind, args) in calls {
        let items = gh::parse_items(&gh::gh(&args)?, kind, "")?;
        full |= items.len() >= limit;
        lists.push(items);
    }
    Ok((merge(lists), not_shown, full))
}

/// One section's list: a page per search (per favorites chunk), hidden repos removed, and when that
/// leaves few rows while more exist, one bigger request instead.
pub fn fetch_section(
    sec: Section,
    tab: usize,
    scope: &Scope,
    favorites: &[String],
    hidden: &ReposCfg,
) -> Result<GlobalList, String> {
    let (items, not_shown, full) = run(sec, tab, scope, favorites, PAGE)?;
    let (kept, dropped) = without_hidden(items, hidden);
    if dropped > 0 && kept.len() < FILL && full {
        let (items, not_shown, _) = run(sec, tab, scope, favorites, TOP_UP)?;
        let (kept, dropped) = without_hidden(items, hidden);
        return Ok(GlobalList {
            items: kept,
            hidden: dropped,
            not_shown,
        });
    }
    Ok(GlobalList {
        items: kept,
        hidden: dropped,
        not_shown,
    })
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
                url: format!("https://github.com/{name}"),
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
        for s in ["all", "favorites", "org:cli", "repo:cli/cli", "org:my-org"] {
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
        let rows = repo_items(&names, &known, &cfg, 1_800_000_000);
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
        let l = fetch_section(Section::MyPrs, 0, &Scope::All, &[], &cfg).unwrap();
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
        let l = fetch_section(Section::MyPrs, 0, &Scope::All, &[], &ReposCfg::default()).unwrap();
        assert_eq!((shim.calls().len(), l.hidden), (1, 0));
        shim.clear("calls.log");
        shim.set(
            "searchprs.out",
            &search_json(&[("noisy/bot", 1), ("keep/me", 2)]),
        );
        let l = fetch_section(Section::MyPrs, 0, &Scope::All, &[], &cfg).unwrap();
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
        )
        .unwrap();
        let c = shim.calls();
        assert_eq!(c.len(), 2);
        assert!(c[0].starts_with("search prs") && c[1].starts_with("search issues"));
        assert!(c[1].ends_with("-- involves:@me is:open archived:false org:cli"));
        assert_eq!(l.items[0].kind, Kind::Pr);
        shim.set("searchprs.err", "HTTP 422: Validation Failed");
        assert!(
            fetch_section(Section::Involved, 0, &Scope::All, &[], &ReposCfg::default()).is_err()
        );
    }
}
