//! The on-disk cache of PR and issue details and lists, on top of `cache::Store`.
//!
//! Details (comments, diffs, commits, checks, overview) are stamped with the item's `updatedAt` as the
//! list showed it, so an entry is exact until the list says the item changed; what `updatedAt` does
//! not track (CI results, mergeability) gets a short TTL instead. Lists are shown from disk at once
//! and refreshed in the background once they pass their TTL. Everything is stored per host and login,
//! and read back through the same cleaning as fresh data: the directory is not trusted.
use crate::cache::Store;
use crate::config::CacheCfg;
use crate::gh::{self, Author, Data, Item, Kind, Label, Tab};
use serde::{Deserialize, Serialize};

/// Largest detail entry written (a huge diff is simply not kept).
const MAX_DETAIL_BYTES: usize = 4 * 1024 * 1024;
/// Detail files kept; the oldest go first.
pub const KEEP_DETAILS: usize = 400;
/// List files kept.
pub const KEEP_LISTS: usize = 200;
pub const DETAIL_PREFIX: &str = "d-";
pub const LIST_PREFIX: &str = "l-";

/// Items whose details are cached and revalidated by `updatedAt`; other kinds keep the old
/// behaviour (loaded once per session, `r` to reload).
pub fn cacheable(kind: Kind) -> bool {
    matches!(kind, Kind::Pr | Kind::Issue)
}

/// Is a copy fetched `age` seconds ago, when the item's `updatedAt` was `stamp`, still good for an
/// item the list now shows as updated at `now_updated`?
pub fn detail_fresh(
    cfg: &CacheCfg,
    tab: Tab,
    data: &Data,
    stamp: &str,
    now_updated: &str,
    age: u64,
) -> bool {
    let known = !stamp.is_empty() && !now_updated.is_empty();
    if known && stamp != now_updated {
        return false;
    }
    let pending = matches!(data, Data::Checks(c) if c.iter().any(|c| c.bucket == "pending"));
    let ttl = match tab {
        Tab::Overview => cfg.overview_s,
        Tab::Checks if pending => cfg.checks_s,
        Tab::Checks => cfg.overview_s,
        _ => cfg.detail_s,
    };
    // without a stamp nothing says the item is unchanged: only a short copy is trusted
    let ttl = if known { ttl } else { ttl.min(cfg.overview_s) };
    age <= ttl
}

/// Data worth keeping: not an error, and a Comments tab only once every page has arrived.
fn persistable(d: &Data) -> bool {
    match d {
        Data::Text(_) => false,
        Data::Comments(c) => c.pend.is_empty() && !c.loading && c.error.is_none(),
        _ => true,
    }
}

#[derive(Serialize, Deserialize)]
struct StoredDetail {
    host: String,
    viewer: String,
    stamp: String,
    data: Data,
}

fn safe(s: &str) -> String {
    s.replace('/', "_")
}

fn detail_key(host: &str, login: &str, repo: &str, kind: Kind, number: u64, tab: Tab) -> String {
    format!(
        "{DETAIL_PREFIX}{host}-{login}-{}-{kind:?}{number}-{tab:?}",
        safe(repo)
    )
}

pub struct Detail {
    pub data: Data,
    /// The item's `updatedAt` when this copy was fetched.
    pub stamp: String,
    pub age: u64,
}

/// A stored detail for this host and login, cleaned again. `floor` ignores entries stored before it
/// (the user asked for a reload).
pub fn read_detail(
    repo: &str,
    kind: Kind,
    number: u64,
    tab: Tab,
    now: u64,
    floor: u64,
) -> Option<Detail> {
    if !cacheable(kind) {
        return None;
    }
    let (host, login) = gh::identity()?;
    let st = Store::default_if_enabled()?;
    let (mut d, age): (StoredDetail, u64) =
        st.read(&detail_key(&host, &login, repo, kind, number, tab), now)?;
    if d.host != host || !d.viewer.eq_ignore_ascii_case(&login) || now.saturating_sub(age) < floor {
        return None;
    }
    gh::reclean(&mut d.data);
    Some(Detail {
        data: d.data,
        stamp: d.stamp,
        age,
    })
}

pub fn write_detail(
    repo: &str,
    kind: Kind,
    number: u64,
    tab: Tab,
    stamp: &str,
    data: &Data,
    now: u64,
) {
    if !cacheable(kind) || !persistable(data) {
        return;
    }
    let (Some((host, login)), Some(st)) = (gh::identity(), Store::default_if_enabled()) else {
        return;
    };
    // serialized through a borrowed copy: the entry owns nothing but the data it points at
    #[derive(Serialize)]
    struct Out<'a> {
        host: &'a str,
        viewer: &'a str,
        stamp: &'a str,
        data: &'a Data,
    }
    let out = Out {
        host: &host,
        viewer: &login,
        stamp,
        data,
    };
    let _ = st.write_limited(
        &detail_key(&host, &login, repo, kind, number, tab),
        &out,
        now,
        MAX_DETAIL_BYTES,
    );
}

/// What a list row keeps on disk (the rest of an `Item` is derived).
#[derive(Serialize, Deserialize)]
struct Row {
    number: u64,
    title: String,
    url: String,
    state: String,
    body: String,
    author: String,
    labels: Vec<String>,
    draft: bool,
    updated: String,
    repo: String,
    issue: bool,
}

#[derive(Serialize, Deserialize)]
struct StoredList {
    host: String,
    viewer: String,
    rows: Vec<Row>,
    hidden: usize,
    not_shown: usize,
}

pub struct CachedList {
    pub items: Vec<Item>,
    pub hidden: usize,
    pub not_shown: usize,
    pub age: u64,
}

fn list_file(host: &str, login: &str, key: &str) -> String {
    format!("{LIST_PREFIX}{host}-{login}-{key}")
}

/// PR and issue lists as cached for this host and login, cleaned again.
pub fn read_list(key: &str, now: u64) -> Option<CachedList> {
    let (host, login) = gh::identity()?;
    let st = Store::default_if_enabled()?;
    let (l, age): (StoredList, u64) = st.read(&list_file(&host, &login, key), now)?;
    if l.host != host || !l.viewer.eq_ignore_ascii_case(&login) {
        return None;
    }
    let items = l
        .rows
        .into_iter()
        .map(|r| {
            let mut it = Item {
                number: r.number,
                title: r.title,
                url: r.url,
                state: r.state,
                body: r.body,
                author: Author { login: r.author },
                labels: r.labels.into_iter().map(|name| Label { name }).collect(),
                is_draft: r.draft,
                updated: r.updated,
                repo: r.repo,
                kind: if r.issue { Kind::Issue } else { Kind::Pr },
                ..Default::default()
            };
            gh::clean_item(&mut it);
            it
        })
        .collect();
    Some(CachedList {
        items,
        hidden: l.hidden,
        not_shown: l.not_shown,
        age,
    })
}

/// A list that is partial (a search failed) is never passed in: it would pass for complete.
pub fn write_list(key: &str, items: &[Item], hidden: usize, not_shown: usize, now: u64) {
    // only PR and issue rows round-trip; a list of anything else is not kept
    if items
        .iter()
        .any(|i| !matches!(i.kind, Kind::Pr | Kind::Issue))
    {
        return;
    }
    let (Some((host, login)), Some(st)) = (gh::identity(), Store::default_if_enabled()) else {
        return;
    };
    let l = StoredList {
        host: host.clone(),
        viewer: login.clone(),
        rows: items
            .iter()
            .map(|i| Row {
                number: i.number,
                title: i.title.clone(),
                url: i.url.clone(),
                state: i.state.clone(),
                body: i.body.clone(),
                author: i.author.login.clone(),
                labels: i.labels.iter().map(|l| l.name.clone()).collect(),
                draft: i.is_draft,
                updated: i.updated.clone(),
                repo: i.repo.clone(),
                issue: i.kind == Kind::Issue,
            })
            .collect(),
        hidden,
        not_shown,
    };
    let _ = st.write(&list_file(&host, &login, key), &l, now);
}

/// A short stable digest for cache keys (favorites, hidden repos, a custom filter).
pub fn digest<I: IntoIterator<Item = S>, S: AsRef<str>>(parts: I) -> String {
    // FNV-1a: no dependency, stable across runs (unlike the std hasher)
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.as_ref().bytes().chain(std::iter::once(0xff)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// Drop old and surplus detail and list files (once per run, off the UI thread).
pub fn prune() {
    if let Some(st) = Store::default_if_enabled() {
        st.prune(DETAIL_PREFIX, KEEP_DETAILS);
        st.prune(LIST_PREFIX, KEEP_LISTS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gh::{Check, Overview};

    fn cfg() -> CacheCfg {
        CacheCfg::default()
    }

    fn checks(bucket: &str) -> Data {
        Data::Checks(vec![Check {
            bucket: bucket.into(),
            ..Default::default()
        }])
    }

    #[test]
    fn a_detail_is_fresh_while_the_item_is_unchanged_and_within_its_tier() {
        let c = cfg();
        let ov = Data::Overview(Overview::default());
        let t = "2026-10-03T10:00:00Z";
        // comments / diff / commits: valid while updatedAt matches, up to a day
        assert!(detail_fresh(
            &c,
            Tab::Comments,
            &Data::Text(vec![]),
            t,
            t,
            80_000
        ));
        assert!(!detail_fresh(
            &c,
            Tab::Comments,
            &Data::Text(vec![]),
            t,
            t,
            90_000
        ));
        // the list now shows a newer update: stale at any age
        assert!(!detail_fresh(
            &c,
            Tab::Comments,
            &Data::Text(vec![]),
            t,
            "2026-10-03T11:00:00Z",
            1
        ));
        // overview: mergeability can move without an update, so five minutes
        assert!(detail_fresh(&c, Tab::Overview, &ov, t, t, 299));
        assert!(!detail_fresh(&c, Tab::Overview, &ov, t, t, 301));
        // checks: 45 s while one is pending, the overview tier once all are done
        assert!(detail_fresh(&c, Tab::Checks, &checks("pending"), t, t, 44));
        assert!(!detail_fresh(&c, Tab::Checks, &checks("pending"), t, t, 46));
        assert!(detail_fresh(&c, Tab::Checks, &checks("pass"), t, t, 200));
        // no updatedAt to compare: only a short copy is trusted, even for a diff
        assert!(detail_fresh(
            &c,
            Tab::Diff,
            &Data::Text(vec![]),
            "",
            "",
            200
        ));
        assert!(!detail_fresh(
            &c,
            Tab::Diff,
            &Data::Text(vec![]),
            "",
            "",
            400
        ));
    }

    #[test]
    fn digests_are_stable_and_order_sensitive() {
        assert_eq!(digest(["a", "b"]), digest(["a", "b"]));
        assert_ne!(digest(["a", "b"]), digest(["b", "a"]));
        assert_ne!(digest(["ab"]), digest(["a", "b"]), "the separator counts");
    }

    #[cfg(unix)]
    #[test]
    fn details_round_trip_through_disk_cleaned_and_scoped_to_the_account() {
        use crate::gh::{CommentsData, FilesData, Pending};
        let _shim = crate::testshim::Shim::new();
        let now = 10_000;
        // a diff: lines are cleaned again on the way back in (the disk is not trusted)
        let mut file =
            crate::diff::parse("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n")
                .pop()
                .unwrap();
        file.lines[1].text = "evil\u{202e}text".into();
        let fd = FilesData {
            sha: "abc".into(),
            files: vec![file],
            ..Default::default()
        };
        write_detail("o/r", Kind::Pr, 7, Tab::Diff, "T1", &Data::Files(fd), now);
        let d = read_detail("o/r", Kind::Pr, 7, Tab::Diff, now + 30, 0).unwrap();
        assert_eq!((d.stamp.as_str(), d.age), ("T1", 30));
        let Data::Files(f) = d.data else {
            panic!("files")
        };
        assert!(
            !f.files[0].lines[1].text.contains('\u{202e}'),
            "{:?}",
            f.files[0].lines[1].text
        );
        // an unfinished Comments tab (more pages to come) is not kept; a complete one is
        let mut c = CommentsData::default();
        c.pend = Pending {
            conn: [Some("cursor".into()), None, None],
            replies: vec![],
        };
        write_detail(
            "o/r",
            Kind::Pr,
            7,
            Tab::Comments,
            "T1",
            &Data::Comments(c),
            now,
        );
        assert!(read_detail("o/r", Kind::Pr, 7, Tab::Comments, now, 0).is_none());
        write_detail(
            "o/r",
            Kind::Pr,
            7,
            Tab::Comments,
            "T1",
            &Data::Comments(CommentsData::default()),
            now,
        );
        assert!(read_detail("o/r", Kind::Pr, 7, Tab::Comments, now, 0).is_some());
        // logs and other kinds are not cached; a reload floor ignores older copies
        write_detail("o/r", Kind::Run, 7, Tab::Logs, "", &Data::Text(vec![]), now);
        assert!(read_detail("o/r", Kind::Run, 7, Tab::Logs, now, 0).is_none());
        assert!(read_detail("o/r", Kind::Pr, 7, Tab::Diff, now + 30, now + 1).is_none());
        // another account on the same machine sees nothing
        crate::gh::set_identity(Some(("github.com".into(), "someone-else".into())));
        assert!(read_detail("o/r", Kind::Pr, 7, Tab::Diff, now, 0).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn lists_round_trip_and_only_pr_and_issue_rows_are_kept() {
        let _shim = crate::testshim::Shim::new();
        let row = |kind, n: u64| Item {
            number: n,
            title: format!("t{n}"),
            url: format!("https://github.com/o/r/pull/{n}"),
            repo: "o/r".into(),
            kind,
            updated: "2026-10-03T10:00:00Z".into(),
            author: Author {
                login: "bob".into(),
            },
            labels: vec![Label { name: "bug".into() }],
            is_draft: true,
            ..Default::default()
        };
        write_list("k1", &[row(Kind::Pr, 1), row(Kind::Issue, 2)], 3, 1, 500);
        let l = read_list("k1", 560).unwrap();
        assert_eq!((l.age, l.hidden, l.not_shown), (60, 3, 1));
        assert_eq!(l.items.len(), 2);
        let (a, b) = (&l.items[0], &l.items[1]);
        assert!((a.kind, b.kind) == (Kind::Pr, Kind::Issue) && a.repo == "o/r" && a.is_draft);
        assert!(
            a.author.login == "bob" && a.labels[0].name == "bug" && a.updated.starts_with("2026")
        );
        write_list("k2", &[row(Kind::Run, 1)], 0, 0, 500);
        assert!(read_list("k2", 500).is_none(), "runs are not kept");
        crate::gh::set_identity(Some(("github.com".into(), "other".into())));
        assert!(read_list("k1", 560).is_none());
    }
}
