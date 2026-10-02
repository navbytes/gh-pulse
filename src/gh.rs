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
    args.iter().map(q).collect::<Vec<_>>().join(" ")
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
        Kind::Pr => &[Overview, Checks, Comments, Diff],
        Kind::Issue => &[Overview, Comments],
        Kind::Run => &[Jobs, Logs],
        Kind::Branch => &[Overview, Commits],
        Kind::Release => &[Overview, Assets],
        Kind::Check => &[Logs],
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
}

impl Item {
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
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

fn json<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, String> {
    serde_json::from_str(s).map_err(|e| format!("bad gh output: {e}"))
}

pub fn parse_items(s: &str, kind: Kind, repo: &str) -> Result<Vec<Item>, String> {
    let mut v: Vec<Item> = json(s)?;
    for i in &mut v {
        let r = &i.repository.name_with_owner;
        (i.kind, i.repo) = (kind, if r.is_empty() { repo.into() } else { r.clone() });
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
    Comments(Vec<Entry>),
    Files(FilesData),
    Text(Vec<String>),
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

/// Entries for the Comments tab from the `COMMENTS_Q` GraphQL response.
pub fn parse_comments(graphql: &str, pr: bool) -> Result<Vec<Entry>, String> {
    let v: Value = json(graphql)?;
    if let Some(e) = v["errors"][0]["message"].as_str() {
        return Err(e.to_string());
    }
    let root = &v["data"]["repository"][if pr { "pullRequest" } else { "issue" }];
    let nodes = |k: &str| root[k]["nodes"].as_array().cloned().unwrap_or_default();
    let mut out = vec![];
    for n in nodes("comments") {
        let card = card_from(&n);
        let head = format!("comment by {} on {}", card.author, day(&card.when));
        out.push(Entry {
            head,
            body: card.body.clone(),
            resolved: false,
            thread: None,
            card,
        });
    }
    for n in nodes("reviews") {
        let mut card = card_from(&n);
        card.badge = n["state"].as_str().unwrap_or("").to_string();
        if card.body.is_empty() && card.badge == "COMMENTED" {
            continue; // the inline comments of this review show up as threads
        }
        let head = format!(
            "review {} by {} on {}",
            card.badge,
            card.author,
            day(&card.when)
        );
        out.push(Entry {
            head,
            body: card.body.clone(),
            resolved: false,
            thread: None,
            card,
        });
    }
    for t in nodes("reviewThreads") {
        let mut it = t["comments"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(card_from);
        let Some(mut card) = it.next() else { continue };
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
        out.push(Entry {
            head: format!("thread {}{flags}", card.badge),
            body: card.body.clone(),
            resolved: card.resolved,
            thread: t["id"].as_str().map(str::to_string),
            card,
        });
    }
    Ok(out)
}

const CARD_FIELDS: &str = "author{login} authorAssociation lastEditedAt body reactionGroups{content users(first:10){totalCount nodes{login}}}";

fn comments_query(pr: bool) -> String {
    let c = CARD_FIELDS;
    let body = if pr {
        format!(
            "pullRequest(number:$p){{comments(first:100){{nodes{{createdAt {c}}}}} reviews(first:100){{nodes{{state submittedAt {c}}}}} reviewThreads(first:100){{nodes{{id isResolved isOutdated path line comments(first:50){{nodes{{createdAt {c}}}}}}}}}}}"
        )
    } else {
        format!("issue(number:$p){{comments(first:100){{nodes{{createdAt {c}}}}}}}")
    };
    format!("query($o:String!,$n:String!,$p:Int!){{repository(owner:$o,name:$n){{{body}}}}}")
}

fn fetch_comments(repo: &str, n: &str, pr: bool) -> Result<Vec<Entry>, String> {
    let (owner, name) = repo.split_once('/').ok_or("bad repo")?;
    let out = gh([
        "api",
        "graphql",
        "-f",
        &format!("query={}", comments_query(pr)),
        "-f",
        &format!("o={owner}"),
        "-f",
        &format!("n={name}"),
        "-F",
        &format!("p={n}"),
    ])?;
    parse_comments(&out, pr)
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
                &it.meta,
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
            let v: Value = json(&release_view(repo, &it.meta, "assets")?)?;
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
        (Kind::Branch, Tab::Commits) => {
            let v: Value = json(&gh([
                "api",
                &format!("repos/{repo}/commits?sha={}&per_page=30", it.title),
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
