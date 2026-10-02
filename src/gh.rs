use crate::diff;
use serde::Deserialize;
use serde_json::Value;
use std::ffi::OsStr;
use std::process::Command;
use std::sync::Mutex;

static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn cmd_log() -> Vec<String> {
    LOG.lock().map(|l| l.clone()).unwrap_or_default()
}

fn log_cmd(prog: &str, args: &[String]) {
    let mut v = vec![prog.to_string()];
    v.extend_from_slice(args);
    push_log(shell(&v));
}

fn push_log(line: String) {
    if let Ok(mut l) = LOG.lock() {
        l.push(line);
        let n = l.len().saturating_sub(200);
        l.drain(..n);
    }
}

/// The command as a copy-pasteable shell line (also what confirm popups show).
pub fn shell(args: &[String]) -> String {
    let q = |a: &String| {
        let safe = |c: char| c.is_ascii_alphanumeric() || "_-./:=@,+%".contains(c);
        if a.is_empty() || !a.chars().all(safe) {
            format!("'{}'", a.replace('\'', "'\\''").replace('\n', "\\n"))
        } else {
            a.clone()
        }
    };
    // shown to the user before anything runs: make look-alike / invisible characters visible
    crate::sanitize::clean(&args.iter().map(q).collect::<Vec<_>>().join(" ")).into_owned()
}

pub const LIMIT: usize = 100;

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    #[default]
    Status,
    Pr,
    Issue,
    Run,
    Workflow,
    Branch,
    Release,
    Other,
    /// Rows of the derived Files / Checks / Comments panels.
    File,
    Check,
    Comment,
    /// Row of the drill-in Commits panel. sha7 in `state`, full sha in `meta`, author in `author.login`,
    /// ISO date in `body`, (+, -, 0) in `fm`.
    Commit,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tab {
    Overview,
    Checks,
    Comments,
    Diff,
    Jobs,
    Logs,
    Commits,
    Assets,
}

impl Tab {
    pub fn name(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Checks => "Checks",
            Tab::Comments => "Comments",
            Tab::Diff => "Diff",
            Tab::Jobs => "Jobs",
            Tab::Logs => "Logs",
            Tab::Commits => "Commits",
            Tab::Assets => "Assets",
        }
    }
}

pub fn tabs(k: Kind) -> &'static [Tab] {
    use Tab::*;
    match k {
        Kind::Pr => &[Overview, Checks, Comments, Diff, Commits],
        Kind::Issue => &[Overview, Comments],
        Kind::Run => &[Jobs, Logs],
        Kind::Branch => &[Overview, Commits],
        Kind::Release => &[Overview, Assets],
        Kind::Check => &[Logs],
        Kind::Commit => &[Diff],
        _ => &[Overview],
    }
}

/// Every list panel's row; fields a source lacks stay default.
#[derive(Deserialize, Debug, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct Item {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub body: String,
    pub author: Author,
    pub labels: Vec<Label>,
    pub is_draft: bool,
    pub repository: RepoRef,
    #[serde(skip)]
    pub repo: String,
    #[serde(skip)]
    pub kind: Kind,
    /// Source-specific one-liner (workflow/branch/event, PR number for a branch).
    #[serde(skip)]
    pub meta: String,
    /// File rows: (additions, deletions, review threads).
    #[serde(skip)]
    pub fm: (u32, u32, u32),
    /// The exact, unsanitized branch name / release tag. Commands must use this (the displayed
    /// `title`/`meta` have look-alike characters neutralized, so they are not the real name).
    #[serde(skip)]
    pub cmd: String,
}

impl Item {
    /// Branch name or release tag exactly as GitHub has it, for command arguments.
    pub fn cmd_name(&self) -> &str {
        if !self.cmd.is_empty() {
            &self.cmd
        } else if self.kind == Kind::Release {
            &self.meta
        } else {
            &self.title
        }
    }

    pub fn key(&self) -> String {
        format!(
            "{:?}|{}|{}|{}",
            self.kind, self.url, self.number, self.title
        )
    }
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct RepoRef {
    name_with_owner: String,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct Author {
    pub login: String,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct Label {
    pub name: String,
}

pub fn gh<I, S>(args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().collect();
    log_cmd(
        "gh",
        &args
            .iter()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    );
    let o = Command::new("gh")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run gh: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        Err(crate::sanitize::clean(String::from_utf8_lossy(&o.stderr).trim()).into_owned())
    }
}

fn json<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, String> {
    serde_json::from_str(s).map_err(|e| format!("bad gh output: {e}"))
}

pub fn clean_item(i: &mut Item) {
    use crate::sanitize::clean_in_place as c;
    c(&mut i.title);
    c(&mut i.body);
    c(&mut i.meta);
    c(&mut i.author.login);
    i.labels.iter_mut().for_each(|l| c(&mut l.name));
}

pub fn parse_items(s: &str, kind: Kind, repo: &str) -> Result<Vec<Item>, String> {
    let mut v: Vec<Item> = json(s)?;
    for i in &mut v {
        let r = &i.repository.name_with_owner;
        (i.kind, i.repo) = (kind, if r.is_empty() { repo.into() } else { r.clone() });
        clean_item(i);
    }
    Ok(v)
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Run {
    database_id: u64,
    display_title: String,
    status: String,
    conclusion: String,
    workflow_name: String,
    head_branch: String,
    event: String,
    url: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Workflow {
    id: u64,
    name: String,
    state: String,
}

const PR_FIELDS: &str = "number,title,url,state,isDraft,author,labels,body";
const ISSUE_FIELDS: &str = "number,title,url,state,author,labels,body";

fn search(panel: usize, tab: usize) -> Result<Vec<Item>, String> {
    let (sub, kind, fields, f): (_, _, _, &[&str]) = if panel == 1 {
        let f: &[&str] = match tab {
            0 => &["--author=@me"],
            1 => &["--review-requested=@me"],
            2 => &["--involves=@me"],
            _ => &["--author=@me", "--merged"],
        };
        (
            "prs",
            Kind::Pr,
            "number,title,url,state,isDraft,author,labels,body,repository",
            f,
        )
    } else {
        let f: &[&str] = match tab {
            0 => &["--assignee=@me"],
            1 => &["--author=@me"],
            _ => &["--involves=@me"],
        };
        (
            "issues",
            Kind::Issue,
            "number,title,url,state,author,labels,body,repository",
            f,
        )
    };
    let limit = format!("--limit={LIMIT}");
    // `gh search` has no --state=merged (it is --merged), and it conflicts with --state=open.
    let mut a = vec!["search", sub, &limit, "--json", fields];
    if !f.contains(&"--merged") {
        a.push("--state=open");
    }
    a.extend(f);
    parse_items(&gh(a)?, kind, "")
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Notif {
    unread: bool,
    reason: String,
    subject: NSubject,
    repository: NRepo,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct NSubject {
    title: String,
    url: Option<String>,
    #[serde(rename = "type")]
    typ: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct NRepo {
    full_name: String,
}

pub fn parse_notifications(s: &str) -> Result<Vec<Item>, String> {
    Ok(json::<Vec<Notif>>(s)?
        .into_iter()
        .map(|n| {
            let api = n.subject.url.unwrap_or_default();
            let kind = match n.subject.typ.as_str() {
                "PullRequest" => Kind::Pr,
                "Issue" => Kind::Issue,
                _ => Kind::Other,
            };
            let number = api
                .rsplit('/')
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let url = if kind == Kind::Other || api.is_empty() {
                format!("https://github.com/{}", n.repository.full_name)
            } else {
                api.replace("api.github.com/repos/", "github.com/")
                    .replace("/pulls/", "/pull/")
            };
            Item {
                number: if kind == Kind::Other { 0 } else { number },
                title: n.subject.title,
                url,
                state: if n.unread { "unread" } else { "read" }.into(),
                meta: format!("{} · {}", n.repository.full_name, n.reason),
                repo: n.repository.full_name,
                kind,
                ..Default::default()
            }
        })
        .collect())
}

fn releases(repo: &str) -> Result<Vec<Item>, String> {
    let f = "tagName,name,isLatest,isPrerelease,isDraft";
    let limit = format!("--limit={LIMIT}");
    let v: Value = json(&gh(["release", "list", "-R", repo, &limit, "--json", f])?)?;
    Ok(v.as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let tag = r["tagName"].as_str().unwrap_or("");
            let flag = |k: &str, n: &'static str| (r[k] == true).then_some(n);
            let state = flag("isLatest", "latest")
                .or(flag("isPrerelease", "pre-release"))
                .or(flag("isDraft", "draft"));
            Item {
                title: r["name"]
                    .as_str()
                    .filter(|n| !n.is_empty())
                    .unwrap_or(tag)
                    .into(),
                meta: tag.into(),
                cmd: tag.into(),
                state: state.unwrap_or("").into(),
                url: format!("https://github.com/{repo}/releases/tag/{tag}"),
                repo: repo.into(),
                kind: Kind::Release,
                ..Default::default()
            }
        })
        .collect())
}

/// Panels: 0 Status, 1 Pull requests, 2 Issues, 3 Actions, 4 Branches, 5 Releases, 6 Notifications.
/// In the global view panels 1-2 search across all repos and the repo-only ones are empty.
pub fn list(repo: &str, global: bool, panel: usize, tab: usize) -> Result<Vec<Item>, String> {
    let mut v = list_raw(repo, global, panel, tab)?;
    v.iter_mut().for_each(clean_item);
    Ok(v)
}

fn list_raw(repo: &str, global: bool, panel: usize, tab: usize) -> Result<Vec<Item>, String> {
    let limit = format!("--limit={LIMIT}");
    if panel == 6 {
        let all = parse_notifications(&gh(["api", "notifications"])?)?;
        // Scoped to the current repo unless in the global view.
        return Ok(all
            .into_iter()
            .filter(|n| global || n.repo.eq_ignore_ascii_case(repo))
            .collect());
    }
    if global {
        return if matches!(panel, 1 | 2) {
            search(panel, tab)
        } else {
            Ok(vec![])
        };
    }
    match panel {
        0 => Ok(vec![Item {
            title: repo.into(),
            repo: repo.into(),
            ..Default::default()
        }]),
        1 => {
            let f: &[&str] = match tab {
                0 => &["--author=@me"],
                1 => &["--search", "review-requested:@me"],
                2 => &[],
                _ => &["--state=merged"],
            };
            let out = gh(["pr", "list", "-R", repo, &limit, "--json", PR_FIELDS]
                .iter()
                .chain(f))?;
            parse_items(&out, Kind::Pr, repo)
        }
        2 => {
            let f: &[&str] = match tab {
                0 => &["--assignee=@me"],
                1 => &["--author=@me"],
                _ => &[],
            };
            let out = gh(
                ["issue", "list", "-R", repo, &limit, "--json", ISSUE_FIELDS]
                    .iter()
                    .chain(f),
            )?;
            parse_items(&out, Kind::Issue, repo)
        }
        3 if tab == 0 => {
            let fields =
                "databaseId,displayTitle,status,conclusion,workflowName,headBranch,event,url";
            let out = gh(["run", "list", "-R", repo, &limit, "--json", fields])?;
            Ok(json::<Vec<Run>>(&out)?
                .into_iter()
                .map(|r| Item {
                    number: r.database_id,
                    title: r.display_title,
                    state: if r.conclusion.is_empty() {
                        r.status
                    } else {
                        r.conclusion
                    },
                    url: r.url,
                    meta: format!("{} · {} · {}", r.workflow_name, r.head_branch, r.event),
                    repo: repo.into(),
                    kind: Kind::Run,
                    ..Default::default()
                })
                .collect())
        }
        3 => {
            let out = gh(["workflow", "list", "-R", repo, "--json", "id,name,state"])?;
            Ok(json::<Vec<Workflow>>(&out)?
                .into_iter()
                .map(|w| Item {
                    number: w.id,
                    title: w.name,
                    state: w.state,
                    url: format!("https://github.com/{repo}/actions"),
                    repo: repo.into(),
                    kind: Kind::Workflow,
                    ..Default::default()
                })
                .collect())
        }
        4 => branches(repo),
        _ => releases(repo),
    }
}

fn branches(repo: &str) -> Result<Vec<Item>, String> {
    let b: Value = json(&gh([
        "api",
        &format!("repos/{repo}/branches?per_page={LIMIT}"),
    ])?)?;
    let prs: Value = json(&gh([
        "pr",
        "list",
        "-R",
        repo,
        "--limit=100",
        "--json",
        "number,headRefName",
    ])?)?;
    let pr_of = |name: &str| {
        let hit = prs.as_array()?.iter().find(|p| p["headRefName"] == name)?;
        hit["number"].as_u64()
    };
    Ok(b.as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b["name"].as_str())
        .map(|name| Item {
            title: name.into(),
            cmd: name.into(),
            number: pr_of(name).unwrap_or(0),
            url: format!("https://github.com/{repo}/tree/{name}"),
            repo: repo.into(),
            kind: Kind::Branch,
            ..Default::default()
        })
        .collect())
}

#[derive(Deserialize, Default, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Overview {
    pub is_draft: bool,
    pub mergeable: String,
    pub base_ref_name: String,
    pub head_ref_name: String,
    pub review_decision: Option<String>,
    pub merged_at: Option<String>,
    pub review_requests: Vec<ReviewReq>,
    pub latest_reviews: Vec<LatestReview>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct ReviewReq {
    pub login: Option<String>,
    pub name: Option<String>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct LatestReview {
    pub author: Option<Author>,
    pub state: String,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Check {
    pub name: String,
    pub bucket: String,
    pub link: String,
    pub workflow: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Only the path is needed here (per-file thread counts for the Files panel).
#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Thread {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reaction {
    /// GitHub's enum name, e.g. THUMBS_UP.
    pub content: String,
    pub count: u32,
    pub users: Vec<String>,
}

/// One comment as shown in the Comments tab: author, standing, age, edits, reactions, replies.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Card {
    pub author: String,
    /// authorAssociation: OWNER, MEMBER, COLLABORATOR, CONTRIBUTOR, NONE, ...
    pub role: String,
    pub when: String,
    pub edited: bool,
    /// Review state (APPROVED, ...) or `path:line` for a thread.
    pub badge: String,
    pub resolved: bool,
    pub outdated: bool,
    pub body: String,
    pub reactions: Vec<Reaction>,
    pub replies: Vec<Card>,
}

/// A selectable block in the Comments tab.
#[derive(Debug, Clone)]
pub struct Entry {
    /// One-line label for the Comments panel.
    pub head: String,
    pub body: String,
    pub resolved: bool,
    pub thread: Option<String>,
    pub card: Card,
}

pub struct FilesData {
    /// Head sha; anchors inline comments.
    pub sha: String,
    pub files: Vec<diff::File>,
    /// Review threads per path.
    pub threads: std::collections::HashMap<String, u32>,
}

pub enum Data {
    Overview(Overview),
    Checks(Vec<Check>),
    Comments(CommentsData),
    Files(FilesData),
    Commits(Vec<CommitRow>),
    Text(Vec<String>),
}

/// One commit of a PR.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommitRow {
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub when: String,
    pub adds: u32,
    pub dels: u32,
}

fn day(s: &str) -> &str {
    s.get(..10).unwrap_or(s)
}

pub fn parse_threads(graphql: &str) -> Result<Vec<Thread>, String> {
    let v: Value = json(graphql)?;
    let nodes = v.pointer("/data/repository/pullRequest/reviewThreads/nodes");
    serde_json::from_value(nodes.cloned().unwrap_or_default()).map_err(|e| e.to_string())
}

fn card_from(n: &Value) -> Card {
    let s = |k: &str| n[k].as_str().unwrap_or("").to_string();
    let reactions = n["reactionGroups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| {
            let count = g["users"]["totalCount"].as_u64().unwrap_or(0) as u32;
            (count > 0).then(|| Reaction {
                content: g["content"].as_str().unwrap_or("").to_string(),
                count,
                users: g["users"]["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|u| u["login"].as_str().map(str::to_string))
                    .collect(),
            })
        })
        .collect();
    Card {
        author: n["author"]["login"].as_str().unwrap_or("ghost").to_string(),
        role: s("authorAssociation"),
        when: if n["createdAt"].is_string() {
            s("createdAt")
        } else {
            s("submittedAt")
        },
        edited: n["lastEditedAt"].is_string(),
        body: s("body"),
        reactions,
        ..Default::default()
    }
}

/// Cursors for what is still to be fetched after the first page of comments.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pending {
    /// Next cursor per connection: issue/PR comments, reviews, review threads.
    pub conn: [Option<String>; 3],
    /// (thread id, cursor) of threads with more than the first 50 replies.
    pub replies: Vec<(String, String)>,
}

impl Pending {
    pub fn is_empty(&self) -> bool {
        self.conn.iter().all(Option::is_none) && self.replies.is_empty()
    }
}

/// The Comments tab data: entries (comments, then reviews, then threads) plus paging state.
#[derive(Debug, Default)]
pub struct CommentsData {
    pub entries: Vec<Entry>,
    /// How many entries each group (comments / reviews / threads) holds, for inserting later pages.
    pub groups: [usize; 3],
    pub pend: Pending,
    /// Raw items fetched / expected, counting skipped empty reviews and thread replies.
    pub loaded: usize,
    pub total: usize,
    /// Sum of thread reply totals (not part of the connection totals).
    pub reply_total: usize,
    pub loading: bool,
    pub error: Option<String>,
    /// Set after a rate-limit error: no new page is requested before this instant.
    pub blocked_until: Option<std::time::Instant>,
}

impl From<Vec<Entry>> for CommentsData {
    fn from(entries: Vec<Entry>) -> Self {
        CommentsData {
            loaded: entries.len(),
            total: entries.len(),
            entries,
            ..Default::default()
        }
    }
}

impl std::ops::Deref for CommentsData {
    type Target = Vec<Entry>;
    fn deref(&self) -> &Vec<Entry> {
        &self.entries
    }
}

impl std::ops::DerefMut for CommentsData {
    fn deref_mut(&mut self) -> &mut Vec<Entry> {
        &mut self.entries
    }
}

/// A background page of comments, merged into the cached `CommentsData` as it arrives. Each carries
/// `rest`, the cursors still to fetch, so loading can pause and resume.
pub enum More {
    Entries {
        group: usize,
        entries: Vec<Entry>,
        loaded: usize,
        total_add: usize,
        rest: Pending,
    },
    Replies {
        thread: String,
        cards: Vec<Card>,
        loaded: usize,
        rest: Pending,
    },
    /// The automatic page budget is used up; more are fetched on demand.
    Paused(Pending),
    Done,
    Failed(String, Pending),
    /// GitHub is throttling us: do not retry before this many seconds pass.
    RateLimited(u64, Pending),
}

impl CommentsData {
    /// Entries still not shown (for the "N more" row).
    pub fn remaining(&self) -> usize {
        self.total.saturating_sub(self.loaded)
    }

    /// More pages exist, nothing is loading, and the last attempt did not fail.
    pub fn paused(&self) -> bool {
        !self.loading && self.error.is_none() && !self.pend.is_empty()
    }

    /// Seconds until a rate-limited retry makes sense (0 when it does now).
    pub fn wait_secs(&self) -> u64 {
        match self.blocked_until {
            Some(t) if t > std::time::Instant::now() => {
                t.duration_since(std::time::Instant::now()).as_secs() + 1
            }
            _ => 0,
        }
    }

    pub fn apply(&mut self, m: More) {
        match m {
            More::Entries {
                group,
                mut entries,
                loaded,
                total_add,
                rest,
            } => {
                // every path that adds text to the screen is cleaned here, not by its caller
                entries.iter_mut().for_each(clean_entry);
                let at = self.groups[..=group].iter().sum::<usize>(); // end of the group
                let n = entries.len();
                self.entries.splice(at..at, entries);
                self.groups[group] += n;
                self.loaded += loaded;
                self.total += total_add;
                self.pend = rest;
            }
            More::Replies {
                thread,
                mut cards,
                loaded,
                rest,
            } => {
                cards.iter_mut().for_each(clean_card);
                if let Some(e) = self
                    .entries
                    .iter_mut()
                    .find(|e| e.thread.as_deref() == Some(&thread))
                {
                    e.card.replies.extend(cards);
                }
                self.loaded += loaded;
                self.pend = rest;
            }
            More::Paused(rest) => (self.loading, self.pend) = (false, rest),
            More::Done => {
                (self.loading, self.pend, self.error, self.blocked_until) =
                    (false, Pending::default(), None, None)
            }
            More::Failed(e, rest) => (self.loading, self.error, self.pend) = (false, Some(e), rest),
            More::RateLimited(secs, rest) => {
                self.loading = false;
                self.pend = rest;
                self.error = Some("GitHub rate limit".into());
                self.blocked_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(secs));
            }
        }
    }
}

fn comment_entry(n: &Value) -> Entry {
    let card = card_from(n);
    let head = format!("comment by {} on {}", card.author, day(&card.when));
    Entry {
        head,
        body: card.body.clone(),
        resolved: false,
        thread: None,
        card,
    }
}

fn review_entry(n: &Value) -> Option<Entry> {
    let mut card = card_from(n);
    card.badge = n["state"].as_str().unwrap_or("").to_string();
    if card.body.is_empty() && card.badge == "COMMENTED" {
        return None; // the inline comments of this review show up as threads
    }
    let head = format!(
        "review {} by {} on {}",
        card.badge,
        card.author,
        day(&card.when)
    );
    Some(Entry {
        head,
        body: card.body.clone(),
        resolved: false,
        thread: None,
        card,
    })
}

fn thread_entry(t: &Value) -> Option<Entry> {
    let mut it = t["comments"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(card_from);
    let mut card = it.next()?;
    card.replies = it.collect();
    let line = t["line"]
        .as_u64()
        .map(|l| format!(":{l}"))
        .unwrap_or_default();
    let path = t["path"].as_str().unwrap_or("");
    (card.resolved, card.outdated) = (t["isResolved"] == true, t["isOutdated"] == true);
    card.badge = format!("{path}{line}");
    let flags = [(card.resolved, " resolved"), (card.outdated, " outdated")]
        .iter()
        .filter(|f| f.0)
        .map(|f| f.1)
        .collect::<String>();
    Some(Entry {
        head: format!("thread {}{flags}", card.badge),
        body: card.body.clone(),
        resolved: card.resolved,
        thread: t["id"].as_str().map(str::to_string),
        card,
    })
}

fn next_cursor(conn: &Value) -> Option<String> {
    (conn["pageInfo"]["hasNextPage"] == true)
        .then(|| conn["pageInfo"]["endCursor"].as_str().map(str::to_string))
        .flatten()
}

/// Comments data from a `comments_query` response (a later page holds just one connection; the
/// missing ones simply parse as empty).
pub fn parse_comments(graphql: &str, pr: bool) -> Result<CommentsData, String> {
    let v: Value = json(graphql)?;
    if let Some(e) = v["errors"][0]["message"].as_str() {
        return Err(e.to_string());
    }
    let root = &v["data"]["repository"][if pr { "pullRequest" } else { "issue" }];
    let nodes = |k: &str| root[k]["nodes"].as_array().cloned().unwrap_or_default();
    let mut cd = CommentsData::default();
    for (i, k) in ["comments", "reviews", "reviewThreads"].iter().enumerate() {
        cd.pend.conn[i] = next_cursor(&root[k]);
        cd.total += root[k]["totalCount"].as_u64().unwrap_or(0) as usize;
        cd.loaded += nodes(k).len();
    }
    for n in nodes("comments") {
        cd.entries.push(comment_entry(&n));
    }
    cd.groups[0] = cd.entries.len();
    for n in nodes("reviews") {
        cd.entries.extend(review_entry(&n));
    }
    cd.groups[1] = cd.entries.len() - cd.groups[0];
    for t in nodes("reviewThreads") {
        let replies = &t["comments"];
        cd.reply_total += replies["totalCount"].as_u64().unwrap_or(0) as usize;
        cd.loaded += replies["nodes"].as_array().map_or(0, Vec::len);
        if let (Some(c), Some(id)) = (next_cursor(replies), t["id"].as_str()) {
            cd.pend.replies.push((id.to_string(), c));
        }
        cd.entries.extend(thread_entry(&t));
    }
    cd.groups[2] = cd.entries.len() - cd.groups[0] - cd.groups[1];
    cd.total += cd.reply_total;
    Ok(cd)
}

const CARD_FIELDS: &str = "author{login} authorAssociation lastEditedAt body reactionGroups{content users(first:25){totalCount nodes{login}}}";

const PAGE: &str = "totalCount pageInfo{hasNextPage endCursor}";

/// One connection's selection. `after` pages it; the first request omits it.
fn conn_query(name: &str, after: bool) -> String {
    let c = CARD_FIELDS;
    let a = if after { ",after:$a" } else { "" };
    match name {
        "comments" => format!("comments(first:100{a}){{{PAGE} nodes{{createdAt {c}}}}}"),
        "reviews" => format!("reviews(first:100{a}){{{PAGE} nodes{{state submittedAt {c}}}}}"),
        _ => format!(
            "reviewThreads(first:100{a}){{{PAGE} nodes{{id isResolved isOutdated path line comments(first:50){{{PAGE} nodes{{createdAt {c}}}}}}}}}"
        ),
    }
}

/// `only` restricts to one connection (a follow-up page); None is the first request for everything.
fn comments_query(pr: bool, only: Option<&str>) -> String {
    let after = only.is_some();
    let body = match (pr, only) {
        (true, Some(c)) => format!("pullRequest(number:$p){{{}}}", conn_query(c, after)),
        (true, None) => format!(
            "pullRequest(number:$p){{{} {} {}}}",
            conn_query("comments", false),
            conn_query("reviews", false),
            conn_query("reviewThreads", false)
        ),
        (false, _) => format!("issue(number:$p){{{}}}", conn_query("comments", after)),
    };
    let a = if after { ",$a:String" } else { "" };
    format!("query($o:String!,$n:String!,$p:Int!{a}){{repository(owner:$o,name:$n){{{body}}}}}")
}

fn graphql_comments(
    repo: &str,
    n: &str,
    pr: bool,
    only: Option<&str>,
    after: Option<&str>,
) -> Result<String, String> {
    let (owner, name) = repo.split_once('/').ok_or("bad repo")?;
    let mut a = vec![
        "api".to_string(),
        "graphql".into(),
        "-f".into(),
        format!("query={}", comments_query(pr, only)),
        "-f".into(),
        format!("o={owner}"),
        "-f".into(),
        format!("n={name}"),
        "-F".into(),
        format!("p={n}"),
    ];
    if let Some(c) = after {
        a.extend(["-f".to_string(), format!("a={c}")]);
    }
    gh(a)
}

fn fetch_comments(repo: &str, n: &str, pr: bool) -> Result<CommentsData, String> {
    let mut cd = parse_comments(&graphql_comments(repo, n, pr, None, None)?, pr)?;
    cd.loading = !cd.pend.is_empty();
    Ok(cd)
}

const REPLIES_Q: &str = "query($id:ID!,$a:String){node(id:$id){... on PullRequestReviewThread{comments(first:50,after:$a){totalCount pageInfo{hasNextPage endCursor} nodes{createdAt CARD}}}}}";

/// (cards, next cursor) from a `REPLIES_Q` response.
pub fn parse_replies(graphql: &str) -> Result<(Vec<Card>, Option<String>), String> {
    let v: Value = json(graphql)?;
    if let Some(e) = v["errors"][0]["message"].as_str() {
        return Err(e.to_string());
    }
    let conn = &v["data"]["node"]["comments"];
    let cards = conn["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(card_from)
        .collect();
    Ok((cards, next_cursor(conn)))
}

/// How many extra pages load by themselves after the first; the rest wait for the user. (Each page is
/// one GraphQL request, and thousands of comments in one burst hit GitHub's secondary rate limit.)
pub const AUTO_PAGES: usize = 5;
/// Minimum gap between page requests.
pub const PAGE_GAP: std::time::Duration = std::time::Duration::from_millis(300);

enum Work {
    Conn(usize, String),
    Replies(String, String),
}

/// The next request in the fixed order: comments, reviews, threads, then long threads' replies.
fn next_work(p: &Pending, pr: bool) -> Option<Work> {
    let conns = if pr { 3 } else { 1 };
    (0..conns)
        .find_map(|g| p.conn[g].clone().map(|c| Work::Conn(g, c)))
        .or_else(|| {
            p.replies
                .first()
                .map(|(id, c)| Work::Replies(id.clone(), c.clone()))
        })
}

/// Seconds to wait when an error really is GitHub throttling (None for everything else: a plain
/// 403 or "could not resolve to a repository" is a real error and is shown as such).
pub fn rate_limit_secs(e: &str) -> Option<u64> {
    let l = e.to_lowercase();
    let hit = [
        "rate limit",
        "abuse detection",
        "retry-after",
        "retry after",
    ]
    .iter()
    .any(|k| l.contains(k));
    hit.then(|| {
        l.split("retry-after")
            .nth(1)
            .or_else(|| l.split("retry after").nth(1))
            .and_then(|t| {
                t.trim_start_matches([':', ' ', '='])
                    .split(|c: char| !c.is_ascii_digit())
                    .next()?
                    .parse()
                    .ok()
            })
            .unwrap_or(60)
    })
}

/// Fetch up to `budget` more pages (throttled), sending each as it arrives, then `Paused` if cursors
/// remain, `Done` if not. Returns early when `send` says the receiver is gone.
pub fn more_pages(
    repo: &str,
    n: &str,
    pr: bool,
    mut pend: Pending,
    budget: usize,
    send: &mut dyn FnMut(More) -> bool,
) {
    let mut fetched = 0;
    loop {
        let Some(work) = next_work(&pend, pr) else {
            send(More::Done);
            return;
        };
        if fetched >= budget {
            send(More::Paused(pend));
            return;
        }
        if fetched > 0 {
            std::thread::sleep(PAGE_GAP);
        }
        fetched += 1;
        let fail = |e: String, rest: Pending| match rate_limit_secs(&e) {
            Some(secs) => More::RateLimited(secs, rest),
            None => More::Failed(e, rest),
        };
        let msg = match work {
            Work::Conn(g, c) => {
                let name = ["comments", "reviews", "reviewThreads"][g];
                match graphql_comments(repo, n, pr, Some(name), Some(&c))
                    .and_then(|j| parse_comments(&j, pr))
                {
                    Err(e) => fail(e, pend.clone()),
                    Ok(cd) => {
                        pend.conn[g] = cd.pend.conn[g].clone();
                        pend.replies.extend(cd.pend.replies.clone());
                        More::Entries {
                            group: g,
                            entries: cd.entries,
                            loaded: cd.loaded,
                            total_add: cd.reply_total,
                            rest: pend.clone(),
                        }
                    }
                }
            }
            Work::Replies(id, c) => {
                let q = REPLIES_Q.replace("CARD", CARD_FIELDS);
                let out = gh([
                    "api",
                    "graphql",
                    "-f",
                    &format!("query={q}"),
                    "-f",
                    &format!("id={id}"),
                    "-f",
                    &format!("a={c}"),
                ]);
                match out.and_then(|o| parse_replies(&o)) {
                    Err(e) => fail(e, pend.clone()),
                    Ok((cards, next)) => {
                        match next {
                            Some(c) => pend.replies[0].1 = c,
                            None => {
                                pend.replies.remove(0);
                            }
                        }
                        let loaded = cards.len();
                        More::Replies {
                            thread: id,
                            cards,
                            loaded,
                            rest: pend.clone(),
                        }
                    }
                }
            }
        };
        let stop = matches!(msg, More::Failed(..) | More::RateLimited(..));
        if !send(msg) || stop {
            return;
        }
    }
}

pub fn epoch(s: &str) -> Option<i64> {
    let n = |a, b| s.get(a..b)?.parse::<i64>().ok();
    let (y, m, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let y = if m <= 2 { y - 1 } else { y };
    let (era, mp) = (y.div_euclid(400), if m > 2 { m - 3 } else { m + 9 });
    let yoe = y - era * 400;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + n(11, 13)? * 3600 + n(14, 16)? * 60 + n(17, 19)?)
}

pub fn duration(c: &Check) -> Option<String> {
    let d = epoch(c.completed_at.as_deref()?)? - epoch(c.started_at.as_deref()?)?;
    (0..86_400)
        .contains(&d)
        .then(|| format!("{}m{:02}s", d / 60, d % 60))
}

/// (run id, job id) from a check link like .../actions/runs/RUN/job/JOB.
pub fn run_ids(link: &str) -> Option<(&str, Option<&str>)> {
    let rest = link.split("/actions/runs/").nth(1)?;
    let mut p = rest.split("/job/");
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let (run, job) = (p.next()?, p.next());
    (digits(run) && job.is_none_or(digits)).then_some((run, job))
}

pub fn failed_log(repo: &str, link: &str) -> Result<String, String> {
    let (run, job) = run_ids(link).ok_or("no workflow run behind this check")?;
    let mut a = vec!["run", "view", run, "-R", repo, "--log-failed"];
    a.extend(job.map(|j| ["--job", j]).into_iter().flatten());
    gh(a)
}

/// Overview of these kinds is rendered from the list row alone.
pub fn needs_fetch(kind: Kind, tab: Tab) -> bool {
    !matches!(
        (kind, tab),
        (
            Kind::Issue | Kind::Workflow | Kind::Branch | Kind::Other,
            Tab::Overview
        )
    )
}

pub fn detail(repo: &str, it: &Item, tab: Tab) -> Result<Data, String> {
    let mut d = detail_raw(repo, it, tab)?;
    clean_data(&mut d);
    Ok(d)
}

fn clean_entry(e: &mut Entry) {
    use crate::sanitize::clean_in_place as cl;
    cl(&mut e.head);
    cl(&mut e.body);
    clean_card(&mut e.card);
}

fn clean_card(c: &mut Card) {
    use crate::sanitize::clean_in_place as cl;
    cl(&mut c.author);
    cl(&mut c.body);
    cl(&mut c.badge);
    c.reactions
        .iter_mut()
        .for_each(|r| r.users.iter_mut().for_each(cl));
    c.replies.iter_mut().for_each(clean_card);
}

/// Everything fetched for the detail pane is cleaned once here, before any rendering.
fn clean_data(d: &mut Data) {
    use crate::sanitize::clean_in_place as cl;
    match d {
        Data::Overview(o) => {
            for r in &mut o.review_requests {
                r.login.iter_mut().chain(r.name.iter_mut()).for_each(cl);
            }
            for r in &mut o.latest_reviews {
                if let Some(a) = &mut r.author {
                    cl(&mut a.login);
                }
            }
        }
        Data::Checks(c) => c.iter_mut().for_each(|c| {
            cl(&mut c.name);
            c.workflow.iter_mut().for_each(cl);
        }),
        Data::Comments(e) => e.iter_mut().for_each(clean_entry),
        Data::Files(_) => {} // the diff parser cleans each line and path
        Data::Commits(c) => c.iter_mut().for_each(|c| {
            cl(&mut c.subject);
            cl(&mut c.author);
        }),
        Data::Text(t) => t.iter_mut().for_each(cl),
    }
}

fn detail_raw(repo: &str, it: &Item, tab: Tab) -> Result<Data, String> {
    let repo = if it.repo.is_empty() {
        repo
    } else {
        it.repo.as_str()
    };
    let n = it.number.to_string();
    let text = |s: String| Data::Text(s.lines().map(str::to_string).collect());
    match (it.kind, tab) {
        (Kind::Pr, Tab::Overview) => {
            let f = "isDraft,mergeable,baseRefName,headRefName,reviewDecision,mergedAt,reviewRequests,latestReviews";
            Ok(Data::Overview(json(&gh([
                "pr", "view", &n, "-R", repo, "--json", f,
            ])?)?))
        }
        (Kind::Pr, Tab::Checks) => {
            let a = [
                "pr",
                "checks",
                &n,
                "-R",
                repo,
                "--json",
                "name,bucket,link,workflow,startedAt,completedAt",
            ];
            // gh exits non-zero for failing/pending checks but still prints the JSON.
            let o = Command::new("gh")
                .args(a)
                .output()
                .map_err(|e| e.to_string())?;
            match serde_json::from_slice(&o.stdout) {
                Ok(c) => Ok(Data::Checks(c)),
                Err(_) if String::from_utf8_lossy(&o.stderr).contains("no checks reported") => {
                    Ok(Data::Checks(vec![]))
                }
                Err(_) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            }
        }
        (Kind::Pr | Kind::Issue, Tab::Comments) => Ok(Data::Comments(fetch_comments(
            repo,
            &n,
            it.kind == Kind::Pr,
        )?)),
        (Kind::Pr, Tab::Diff) => {
            let sha = gh([
                "pr",
                "view",
                &n,
                "-R",
                repo,
                "--json",
                "headRefOid",
                "-q",
                ".headRefOid",
            ])?;
            // Huge PRs make `gh pr diff` fail (HTTP 406 "too large" / 422); the files API still has per-file patches.
            let d = gh(["pr", "diff", &n, "-R", repo, "--color=never"]).or_else(|e| {
                files_diff(repo, &n).map_err(|e2| format!("{e} (files API fallback failed: {e2})"))
            })?;
            let mut counts = std::collections::HashMap::new();
            for t in threads(repo, &n).unwrap_or_default() {
                *counts.entry(t.path).or_insert(0u32) += 1;
            }
            Ok(Data::Files(FilesData {
                sha: sha.trim().into(),
                files: diff::parse(&d),
                threads: counts,
            }))
        }
        (Kind::Check, Tab::Logs) => {
            let s = failed_log(repo, &it.url)?;
            Ok(if s.trim().is_empty() {
                Data::Text(vec!["(no failed-step log for this check)".into()])
            } else {
                tail(s)
            })
        }
        (Kind::Release, Tab::Overview) => {
            let v: Value = json(&release_view(
                repo,
                it.cmd_name(),
                "name,tagName,publishedAt,body,isPrerelease",
            )?)?;
            let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
            Ok(text(format!(
                "{} ({})\npublished: {}\n\n{}",
                s("name"),
                s("tagName"),
                s("publishedAt"),
                s("body")
            )))
        }
        (Kind::Release, Tab::Assets) => {
            let v: Value = json(&release_view(repo, it.cmd_name(), "assets")?)?;
            Ok(Data::Text(
                v["assets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| {
                        format!(
                            "{}  ({} KB, {} downloads)",
                            a["name"].as_str().unwrap_or("?"),
                            a["size"].as_u64().unwrap_or(0) / 1024,
                            a["downloadCount"]
                        )
                    })
                    .collect(),
            ))
        }
        (Kind::Run, Tab::Jobs) => {
            let v: Value = json(&gh(["run", "view", &n, "-R", repo, "--json", "jobs"])?)?;
            let icon = |o: &Value| match (o["conclusion"].as_str(), o["status"].as_str()) {
                (Some("success"), _) => "✓",
                (Some("failure"), _) => "✗",
                (Some("skipped" | "cancelled"), _) => "-",
                (_, Some("completed")) => "?",
                _ => "●",
            };
            let mut out = vec![];
            for j in v["jobs"].as_array().into_iter().flatten() {
                out.push(format!("{} {}", icon(j), j["name"].as_str().unwrap_or("?")));
                for s in j["steps"].as_array().into_iter().flatten() {
                    out.push(format!(
                        "    {} {}",
                        icon(s),
                        s["name"].as_str().unwrap_or("?")
                    ));
                }
            }
            Ok(Data::Text(out))
        }
        (Kind::Run, Tab::Logs) => {
            let s = gh(["run", "view", &n, "-R", repo, "--log-failed"])?;
            Ok(if s.trim().is_empty() {
                Data::Text(vec!["(no failed steps)".into()])
            } else {
                tail(s)
            })
        }
        (Kind::Pr, Tab::Commits) => Ok(Data::Commits(fetch_pr_commits(repo, &n)?)),
        (Kind::Commit, Tab::Diff) => {
            let sha = it.meta.as_str();
            let path = format!("repos/{repo}/commits/{sha}");
            // The diff media type has size limits; the JSON `files` still carries per-file patches.
            let diff =
                gh(["api", &path, "-H", "Accept: application/vnd.github.diff"]).or_else(|e| {
                    let v: Value = json(&gh(["api", &path])?)?;
                    if v["files"].is_array() {
                        Ok(files_to_diff(&v["files"]))
                    } else {
                        Err(e)
                    }
                })?;
            Ok(Data::Files(FilesData {
                sha: sha.to_string(),
                files: diff::parse(&diff),
                threads: Default::default(),
            }))
        }
        (Kind::Branch, Tab::Commits) => {
            let v: Value = json(&gh([
                "api",
                &format!("repos/{repo}/commits?sha={}&per_page=30", it.cmd_name()),
            ])?)?;
            Ok(Data::Text(
                v.as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| {
                        let msg = c["commit"]["message"]
                            .as_str()
                            .unwrap_or("")
                            .lines()
                            .next()
                            .unwrap_or("");
                        let sha = c["sha"].as_str().unwrap_or("").get(..7).unwrap_or("");
                        format!(
                            "{sha} {msg} ({})",
                            c["commit"]["author"]["name"].as_str().unwrap_or("?")
                        )
                    })
                    .collect(),
            ))
        }
        (Kind::Status, _) => status(repo).map(text),
        _ => Ok(Data::Text(vec![])),
    }
}

fn tail(s: String) -> Data {
    let v: Vec<String> = s.lines().map(str::to_string).collect();
    let skip = v.len().saturating_sub(5000);
    Data::Text(v.into_iter().skip(skip).collect())
}

/// Rebuilds a unified diff from `pulls/{n}/files` (what `gh pr diff` can't serve for huge PRs).
pub fn files_to_diff(files: &Value) -> String {
    let mut out = String::new();
    for f in files.as_array().into_iter().flatten() {
        let s = |k: &str| f[k].as_str().unwrap_or("");
        let (name, status) = (s("filename"), s("status"));
        let old = f["previous_filename"].as_str().unwrap_or(name);
        out += &format!("diff --git a/{old} b/{name}\n");
        if status == "renamed" {
            out += &format!("rename from {old}\nrename to {name}\n");
        }
        match f["patch"].as_str() {
            Some(p) => {
                let a = if status == "added" {
                    "/dev/null".into()
                } else {
                    format!("a/{old}")
                };
                let b = if status == "removed" {
                    "/dev/null".into()
                } else {
                    format!("b/{name}")
                };
                out += &format!("--- {a}\n+++ {b}\n{p}\n");
            }
            None if status == "renamed" => {}
            None => out += &format!("Binary files a/{old} and b/{name} differ\n"),
        }
    }
    out
}

const COMMITS_Q: &str = "query($o:String!,$n:String!,$p:Int!,$a:String){repository(owner:$o,name:$n){pullRequest(number:$p){commits(first:100,after:$a){pageInfo{hasNextPage endCursor} nodes{commit{oid messageHeadline committedDate additions deletions author{name user{login}}}}}}}}";

/// (rows, next cursor) from one GraphQL page of `pullRequest.commits`.
pub fn parse_commits(graphql: &str) -> Result<(Vec<CommitRow>, Option<String>), String> {
    let v: Value = json(graphql)?;
    if let Some(e) = v["errors"][0]["message"].as_str() {
        return Err(e.to_string());
    }
    let c = &v["data"]["repository"]["pullRequest"]["commits"];
    let rows = c["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| {
            let n = &n["commit"];
            let s = |p: &str| {
                n.pointer(p)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let author = n
                .pointer("/author/user/login")
                .and_then(Value::as_str)
                .map_or_else(|| s("/author/name"), str::to_string);
            CommitRow {
                sha: s("/oid"),
                subject: s("/messageHeadline"),
                author,
                when: s("/committedDate"),
                adds: n["additions"].as_u64().unwrap_or(0) as u32,
                dels: n["deletions"].as_u64().unwrap_or(0) as u32,
            }
        })
        .collect();
    let next = (c["pageInfo"]["hasNextPage"] == true)
        .then(|| c["pageInfo"]["endCursor"].as_str().map(str::to_string))
        .flatten();
    Ok((rows, next))
}

/// All commits of a PR (GitHub caps a PR at 250), following cursors.
fn fetch_pr_commits(repo: &str, n: &str) -> Result<Vec<CommitRow>, String> {
    let (owner, name) = repo.split_once('/').ok_or("bad repo")?;
    let (mut all, mut after) = (vec![], None::<String>);
    for _ in 0..4 {
        let mut a = vec![
            "api".to_string(),
            "graphql".into(),
            "-f".into(),
            format!("query={COMMITS_Q}"),
            "-f".into(),
            format!("o={owner}"),
            "-f".into(),
            format!("n={name}"),
            "-F".into(),
            format!("p={n}"),
        ];
        if let Some(c) = &after {
            a.extend(["-f".to_string(), format!("a={c}")]);
        }
        let (rows, next) = parse_commits(&gh(a)?)?;
        all.extend(rows);
        match next {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    Ok(all)
}

fn files_diff(repo: &str, n: &str) -> Result<String, String> {
    let path = format!("repos/{repo}/pulls/{n}/files?per_page=100");
    let pages: Value = json(&gh(["api", &path, "--paginate", "--slurp"])?)?;
    let flat: Vec<Value> = pages
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|p| p.as_array().cloned().unwrap_or_default())
        .collect();
    Ok(files_to_diff(&Value::Array(flat)))
}

fn threads(repo: &str, n: &str) -> Result<Vec<Thread>, String> {
    let (owner, name) = repo.split_once('/').ok_or("bad repo")?;
    let q = "query($o:String!,$n:String!,$p:Int!){repository(owner:$o,name:$n){pullRequest(number:$p){reviewThreads(first:100){nodes{id isResolved isOutdated path line comments(first:50){nodes{author{login} body createdAt}}}}}}}";
    let out = gh([
        "api",
        "graphql",
        "-f",
        &format!("query={q}"),
        "-f",
        &format!("o={owner}"),
        "-f",
        &format!("n={name}"),
        "-F",
        &format!("p={n}"),
    ])?;
    parse_threads(&out)
}

fn status(repo: &str) -> Result<String, String> {
    let f = "nameWithOwner,description,defaultBranchRef,stargazerCount,forkCount,isPrivate,url,viewerPermission";
    let v: Value = json(&gh(["repo", "view", repo, "--json", f])?)?;
    let s = |p: &str| {
        v.pointer(p)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let mut out = format!(
        "{} ({})\n{}\n\ndefault branch: {}\nstars: {}  forks: {}\npermission: {}\n{}\n",
        s("/nameWithOwner"),
        if v["isPrivate"] == true {
            "private"
        } else {
            "public"
        },
        s("/description"),
        s("/defaultBranchRef/name"),
        v["stargazerCount"],
        v["forkCount"],
        s("/viewerPermission"),
        s("/url"),
    );
    if !repo_here().is_ok_and(|h| h.eq_ignore_ascii_case(repo)) {
        // cwd is another repo, so its branch says nothing about this one
        let prs: Value = json(&gh([
            "pr",
            "list",
            "-R",
            repo,
            "--json",
            "number",
            "--limit=100",
        ])?)?;
        let n = prs.as_array().map_or(0, Vec::len);
        out += &format!("\nopen PRs: {n} (cwd is not a clone of {repo})");
        return Ok(out);
    }
    let branch = Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok();
    let branch = branch.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if let Some(b) = branch.filter(|b| !b.is_empty()) {
        let prs = gh([
            "pr",
            "list",
            "-R",
            repo,
            "--head",
            &b,
            "--state=all",
            "--json",
            "number,title,state",
        ])?;
        let prs: Value = json(&prs)?;
        out += &match prs.get(0) {
            Some(p) => format!(
                "\ncurrent branch {b}: PR #{} {} [{}]",
                p["number"],
                p["title"].as_str().unwrap_or(""),
                p["state"].as_str().unwrap_or("")
            ),
            None => format!("\ncurrent branch {b}: no PR"),
        };
    }
    Ok(out)
}

/// Runs a full command line (program first), as built by an Action.
pub fn run(cmd: &[String]) -> Result<String, String> {
    let Some((prog, args)) = cmd.split_first() else {
        return Err("empty command".into());
    };
    log_cmd(prog, args);
    let o = Command::new(prog)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {prog}: {e}"))?;
    let (out, err) = (
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr),
    );
    if o.status.success() {
        Ok(out.lines().last().unwrap_or("done").to_string())
    } else {
        push_log(format!("  error: {}", err.lines().next().unwrap_or("")));
        Err(err.trim().to_string())
    }
}

/// Canonical owner/name, or an error when the repo doesn't exist or isn't accessible.
pub fn resolve_repo(repo: &str) -> Result<String, String> {
    gh([
        "repo",
        "view",
        repo,
        "--json",
        "nameWithOwner",
        "-q",
        ".nameWithOwner",
    ])
    .map(|s| s.trim().to_string())
    .map_err(|_| format!("repo not found or no access: {repo}"))
}

pub fn repo_here() -> Result<String, String> {
    gh([
        "repo",
        "view",
        "--json",
        "nameWithOwner",
        "-q",
        ".nameWithOwner",
    ])
    .map(|s| s.trim().into())
}

pub fn user() -> String {
    gh(["api", "user", "-q", ".login"])
        .map(|s| s.trim().into())
        .unwrap_or_default()
}

/// `--` keeps a tag like "-x" from being parsed as a flag.
fn release_view(repo: &str, tag: &str, fields: &str) -> Result<String, String> {
    gh(["release", "view", "-R", repo, "--json", fields, "--", tag])
}

/// Local-repo actions (checkout, git switch) only make sense when cwd is a clone of `repo`.
pub fn require_clone(repo: &str) -> Result<(), String> {
    if repo_here().is_ok_and(|h| h.eq_ignore_ascii_case(repo)) {
        Ok(())
    } else {
        Err(format!("cwd is not a clone of {repo}"))
    }
}

pub fn checkout(repo: &str, n: u64) -> Result<String, String> {
    require_clone(repo)?;
    gh(["pr", "checkout", &n.to_string(), "-R", repo]).map(|_| format!("checked out #{n}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_api_fallback_builds_a_parsable_diff() {
        let v: Value = serde_json::from_str(
            r#"[{"filename":"a b.txt","status":"modified","patch":"@@ -1 +1 @@\n-x\n+y"},
                {"filename":"new.rs","status":"added","patch":"@@ -0,0 +1 @@\n+fn main(){}"},
                {"filename":"b.rs","previous_filename":"a.rs","status":"renamed"},
                {"filename":"img.png","status":"added"}]"#,
        )
        .unwrap();
        let files = diff::parse(&files_to_diff(&v));
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["a b.txt", "new.rs", "b.rs", "img.png"]);
        assert_eq!(files[0].lines.len(), 3);
        assert_eq!(files[2].lines[0].text, "renamed to b.rs");
    }

    #[test]
    fn github_text_is_neutralized_on_the_way_in() {
        let json = r#"[{"number":1,"title":"fix\u202eevil.txt","body":"x\u001b[2Jy","state":"open","url":"u","author":{"login":"a\u200bb"},"labels":[{"name":"l\u2066"}]}]"#;
        let it = &parse_items(json, Kind::Pr, "o/r").unwrap()[0];
        assert_eq!(it.title, "fix<U+202E>evil.txt");
        assert_eq!(it.body, "x<U+001B>[2Jy");
        assert_eq!(it.author.login, "a<U+200B>b");
        assert_eq!(it.labels[0].name, "l<U+2066>");
        let mut d = Data::Text(vec!["log \u{202e}line".into()]);
        clean_data(&mut d);
        assert!(matches!(&d, Data::Text(t) if t[0] == "log <U+202E>line"));
        let args = vec![
            "gh".to_string(),
            "pr".into(),
            "comment".into(),
            "-b".into(),
            "pay\u{202e}load".into(),
        ];
        assert_eq!(
            shell(&args),
            "gh pr comment -b 'pay<U+202E>load'",
            "the confirm popup shows the real characters"
        );
        let files =
            diff::parse("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\u{202e}c\n");
        assert_eq!(files[0].lines[2].text, "b<U+202E>c");
    }

    #[test]
    fn parses_commit_pages() {
        let (rows, next) = parse_commits(include_str!("../tests/commits.json")).unwrap();
        assert_eq!(next.as_deref(), Some("C2"));
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (
                rows[0].sha.as_str(),
                rows[0].subject.as_str(),
                rows[0].author.as_str(),
                rows[0].adds,
                rows[0].dels
            ),
            (
                "1111111aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "Add the thing",
                "octocat",
                12,
                3
            )
        );
        assert_eq!(
            rows[1].author, "Mona Lisa",
            "no linked user: falls back to the git author name"
        );
        assert!(parse_commits(r#"{"errors":[{"message":"nope"}]}"#).is_err());
    }

    #[test]
    fn comments_page_through_every_connection() {
        let mut cd = parse_comments(include_str!("../tests/comments_p1.json"), true).unwrap();
        assert_eq!(cd.pend.conn, [Some("CC1".into()), None, Some("TC1".into())]);
        assert_eq!(cd.pend.replies, [("T1".to_string(), "RC1".to_string())]);
        assert_eq!((cd.loaded, cd.total, cd.remaining()), (6, 10, 4));
        assert_eq!(cd.groups, [2, 1, 1]);
        cd.loading = true;
        // second page of comments lands after the existing comments, before reviews and threads
        let p = parse_comments(include_str!("../tests/comments_p2.json"), true).unwrap();
        assert_eq!(p.pend.conn, [None, None, None]);
        cd.apply(More::Entries {
            group: 0,
            entries: p.entries,
            loaded: p.loaded,
            total_add: p.reply_total,
            rest: Pending::default(),
        });
        let heads: Vec<_> = cd.iter().map(|e| e.head.as_str()).collect();
        assert_eq!(
            heads[..3],
            [
                "comment by a on 2026-01-01",
                "comment by b on 2026-01-02",
                "comment by c on 2026-01-03"
            ]
        );
        assert!(
            heads[3].starts_with("review ") && heads[4].starts_with("thread "),
            "{heads:?}"
        );
        assert_eq!(cd.groups, [3, 1, 1]);
        // next page of threads (with its own long thread) appends at the end
        let p = parse_comments(include_str!("../tests/comments_p3.json"), true).unwrap();
        assert_eq!(p.pend.replies, [("T2".to_string(), "RC9".to_string())]);
        cd.pend.replies.extend(p.pend.replies.clone());
        cd.apply(More::Entries {
            group: 2,
            entries: p.entries,
            loaded: p.loaded,
            total_add: p.reply_total,
            rest: Pending::default(),
        });
        assert_eq!(cd.last().unwrap().thread.as_deref(), Some("T2"));
        assert_eq!(cd.groups, [3, 1, 2]);
        // extra replies are added to their thread
        let (cards, next) = parse_replies(include_str!("../tests/replies.json")).unwrap();
        assert_eq!((cards.len(), next), (2, None));
        cd.apply(More::Replies {
            thread: "T1".into(),
            cards: cards.clone(),
            loaded: 2,
            rest: Pending::default(),
        });
        cd.apply(More::Replies {
            thread: "T2".into(),
            cards,
            loaded: 2,
            rest: Pending::default(),
        });
        let t1 = cd
            .iter()
            .find(|e| e.thread.as_deref() == Some("T1"))
            .unwrap();
        assert_eq!(
            t1.card.replies.len(),
            1 + 2,
            "2 on the first page become 1 reply + the first comment, plus 2 more"
        );
        assert_eq!(
            cd.remaining(),
            0,
            "every item accounted for: {} / {}",
            cd.loaded,
            cd.total
        );
        cd.apply(More::Done);
        assert!(!cd.loading && cd.pend.is_empty());
        cd.apply(More::Failed("boom".into(), Pending::default()));
        assert_eq!(cd.error.as_deref(), Some("boom"));
    }

    #[test]
    fn paging_order_and_rate_limit_detection() {
        let mut p = Pending {
            conn: [None, Some("r1".into()), Some("t1".into())],
            replies: vec![("T".into(), "c".into())],
        };
        assert!(
            matches!(next_work(&p, true), Some(Work::Conn(1, c)) if c == "r1"),
            "comments, then reviews, then threads, then replies"
        );
        p.conn[1] = None;
        assert!(matches!(next_work(&p, true), Some(Work::Conn(2, _))));
        p.conn[2] = None;
        assert!(matches!(next_work(&p, true), Some(Work::Replies(id, _)) if id == "T"));
        p.replies.clear();
        assert!(next_work(&p, true).is_none());
        let issue = Pending {
            conn: [None, Some("x".into()), None],
            ..Default::default()
        };
        assert!(
            next_work(&issue, false).is_none(),
            "issues only page their comments"
        );
        assert_eq!(
            rate_limit_secs("GraphQL: API rate limit already exceeded for user ID 1"),
            Some(60)
        );
        assert_eq!(
            rate_limit_secs("You have exceeded a secondary rate limit. retry-after: 42"),
            Some(42)
        );
        assert_eq!(
            rate_limit_secs("gh: Resource not accessible by integration (HTTP 403)"),
            None,
            "a plain 403 is a real error"
        );
        assert_eq!(
            rate_limit_secs(
                "GraphQL: Could not resolve to a Repository with the name 'o/r'. (repository)"
            ),
            None,
            "shown as the real error"
        );
        assert_eq!(
            rate_limit_secs(
                "gh: API rate limit exceeded for user ID 1. If you reach out to GitHub Support for help, please include the request ID ABC. (HTTP 403)"
            ),
            Some(60)
        );
        assert_eq!(
            rate_limit_secs(
                "You have exceeded a secondary rate limit. Please wait a few minutes before you try again. (HTTP 403)"
            ),
            Some(60)
        );
        assert_eq!(
            rate_limit_secs("You have triggered an abuse detection mechanism. Retry-After: 17"),
            Some(17)
        );
        assert_eq!(rate_limit_secs("network unreachable"), None);
        // an in-flight / blocked state
        let mut cd = CommentsData {
            pend: Pending {
                conn: [Some("c".into()), None, None],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(cd.paused());
        cd.apply(More::RateLimited(30, cd.pend.clone()));
        assert!(
            !cd.paused() && cd.error.is_some() && (29..=31).contains(&cd.wait_secs()),
            "{}",
            cd.wait_secs()
        );
        cd.apply(More::Paused(cd.pend.clone()));
        assert!(cd.pend.conn[0].is_some(), "cursors survive a pause");
    }

    /// Later pages and thread replies never pass through `detail()`, so `apply` must clean them itself.
    #[test]
    fn later_comment_pages_and_replies_are_sanitized() {
        let mut cd = parse_comments(include_str!("../tests/comments_p1.json"), true).unwrap();
        let p = parse_comments(include_str!("../tests/comments_evil_page.json"), true).unwrap();
        cd.apply(More::Entries {
            group: 0,
            entries: p.entries,
            loaded: 1,
            total_add: 0,
            rest: Pending::default(),
        });
        let e = cd.iter().find(|e| e.card.author.starts_with("ev")).unwrap();
        assert_eq!(e.card.author, "ev<U+202E>il");
        assert_eq!(e.card.body, "click <U+001B>[2J here <U+202E>gnp.exe");
        assert_eq!(e.body, e.card.body);
        assert!(e.head.contains("ev<U+202E>il"), "{}", e.head);
        assert_eq!(e.card.reactions[0].users, ["u<U+200B>ser"]);
        let (cards, _) = parse_replies(include_str!("../tests/replies_evil.json")).unwrap();
        cd.apply(More::Replies {
            thread: "T1".into(),
            cards,
            loaded: 1,
            rest: Pending::default(),
        });
        let t = cd
            .iter()
            .find(|e| e.thread.as_deref() == Some("T1"))
            .unwrap();
        let r = t.card.replies.last().unwrap();
        assert_eq!(
            (r.author.as_str(), r.body.as_str()),
            ("r<U+2066>x", "reply <U+001B>]0;title<U+0007> text")
        );
        let screen: String = cd
            .iter()
            .flat_map(|e| [e.head.clone(), e.body.clone(), e.card.body.clone()])
            .collect();
        assert!(
            !screen
                .chars()
                .any(|c| matches!(c, '\u{1b}' | '\u{202e}' | '\u{200b}'))
        );
    }

    #[test]
    fn issue_comments_query_pages_only_comments() {
        let q = comments_query(false, Some("comments"));
        assert!(
            q.contains("issue(number:$p)")
                && q.contains("after:$a")
                && q.contains("$a:String")
                && !q.contains("reviews")
        );
        assert!(
            comments_query(true, None).contains("reviewThreads(first:100)")
                && !comments_query(true, None).contains("after:$a")
        );
    }

    #[test]
    fn parses_list_json() {
        let items = parse_items(include_str!("../tests/prs.json"), Kind::Pr, "octo/repo").unwrap();
        let i = &items[0];
        assert_eq!(
            (i.number, i.author.login.as_str(), i.labels[0].name.as_str()),
            (42, "octocat", "bug")
        );
        assert_eq!(i.repo, "octo/repo");
    }

    #[test]
    fn parses_threads_for_counts() {
        let t = parse_threads(include_str!("../tests/threads.json")).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].path, "src/x.rs");
    }

    #[test]
    fn parses_comment_cards() {
        let e = parse_comments(include_str!("../tests/comments.json"), true).unwrap();
        let heads: Vec<_> = e.iter().map(|e| e.head.as_str()).collect();
        assert_eq!(
            heads,
            [
                "comment by octocat on 2026-01-02",
                "review APPROVED by hubot on 2026-01-03",
                "thread src/x.rs:12 resolved outdated"
            ],
            "empty COMMENTED reviews are skipped"
        );
        let c = &e[0].card;
        assert_eq!(
            (c.role.as_str(), c.edited, c.body.as_str()),
            ("OWNER", true, "Looks **good**")
        );
        assert_eq!(c.reactions.len(), 2, "zero-count groups are dropped");
        assert_eq!(
            (c.reactions[0].content.as_str(), c.reactions[0].count),
            ("THUMBS_UP", 3)
        );
        assert_eq!(c.reactions[0].users, ["hubot", "monalisa"]);
        let t = &e[2];
        assert!(t.resolved && t.thread.as_deref() == Some("PRRT_1"));
        assert_eq!(
            (t.card.author.as_str(), t.card.badge.as_str()),
            ("alice", "src/x.rs:12")
        );
        assert_eq!(t.card.replies.len(), 1);
        assert_eq!(
            t.card.replies[0].author, "ghost",
            "deleted users render as ghost"
        );
        let issue = parse_comments(r#"{"data":{"repository":{"issue":{"comments":{"nodes":[{"author":{"login":"a"},"authorAssociation":"NONE","createdAt":"2026-02-01T00:00:00Z","lastEditedAt":null,"body":"hi","reactionGroups":[]}]}}}}}"#, false).unwrap();
        assert_eq!(issue.len(), 1);
        assert!(!issue[0].card.edited);
        assert!(parse_comments(r#"{"errors":[{"message":"nope"}]}"#, true).is_err());
    }

    #[test]
    fn parses_checks_and_duration() {
        let c: Vec<Check> = serde_json::from_str(include_str!("../tests/checks.json")).unwrap();
        assert_eq!(c[0].bucket, "pass");
        assert_eq!(duration(&c[0]).as_deref(), Some("1m05s"));
        assert_eq!(duration(&c[1]), None);
        assert_eq!(run_ids(&c[0].link), Some(("111", Some("222"))));
        assert_eq!(run_ids("https://x/actions/runs/1a/job/2"), None);
        assert_eq!(run_ids("https://x/actions/runs/1/job/--x"), None);
        let q = |a: &[&str]| shell(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(q(&["gh", "-b", "a;b `c` *\n#"]), "gh -b 'a;b `c` *\\n#'");
    }

    /// Live: a >300-file PR (gh pr diff refuses) must still produce a diff via the files API.
    /// GH_PULSE_HUGE_PR=owner/repo#N cargo test -- --ignored --nocapture
    #[test]
    #[ignore]
    fn huge_pr_diff_falls_back() {
        let Ok(spec) = std::env::var("GH_PULSE_HUGE_PR") else {
            return;
        };
        let (repo, n) = spec.split_once('#').expect("owner/repo#N");
        let it = Item {
            number: n.parse().unwrap(),
            repo: repo.into(),
            kind: Kind::Pr,
            ..Default::default()
        };
        let Ok(Data::Files(FilesData { files: f, .. })) = detail("", &it, Tab::Diff) else {
            panic!("no diff")
        };
        println!("{} files", f.len());
        assert!(f.len() > 300 && f.iter().any(|f| f.lines.len() > 1));
    }

    /// Live smoke test: GH_PULSE_REPO=o/r cargo test -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_smoke() {
        let repo = std::env::var("GH_PULSE_REPO").unwrap();
        for (panel, tab) in [
            (0, 0),
            (1, 0),
            (1, 1),
            (1, 2),
            (1, 3),
            (2, 0),
            (2, 1),
            (2, 2),
            (3, 0),
            (3, 1),
            (4, 0),
        ] {
            let items = list(&repo, false, panel, tab)
                .unwrap_or_else(|e| panic!("list {panel}/{tab}: {e}"));
            println!("list {panel}/{tab}: {} items", items.len());
            for it in items.iter().take(2) {
                for t in tabs(it.kind) {
                    match detail(&repo, it, *t) {
                        Ok(d) => println!(
                            "  {:?} {:?} ok {}",
                            it.kind,
                            t,
                            match d {
                                Data::Checks(c) => format!("{} checks", c.len()),
                                Data::Comments(e) => format!("{} entries", e.len()),
                                Data::Files(fd) => format!("{} files", fd.files.len()),
                                Data::Commits(c) => format!("{} commits", c.len()),
                                Data::Text(t) => format!("{} lines", t.len()),
                                Data::Overview(_) => "overview".into(),
                            }
                        ),
                        Err(e) => panic!("detail {:?} {:?}: {e}", it.kind, t),
                    }
                }
            }
        }
    }
}
