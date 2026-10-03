use crate::diff;
use serde::Deserialize;
use serde_json::Value;
use std::ffi::{OsStr, OsString};
use std::process::Command;
use std::sync::Mutex;

/// Which program runs as `gh` (tests point this at a shim script).
static PROGRAM: Mutex<Option<OsString>> = Mutex::new(None);

#[cfg(test)]
pub fn set_program(p: Option<OsString>) {
    if let Ok(mut g) = PROGRAM.lock() {
        *g = p;
    }
}

fn program() -> OsString {
    let set = PROGRAM.lock().ok().and_then(|g| g.clone());
    // unit tests never reach the real GitHub by accident (a job outliving its shim would): the
    // `--ignored` live tests opt in with GH_PULSE_LIVE=1
    #[cfg(test)]
    if set.is_none() && std::env::var_os("GH_PULSE_LIVE").is_none() {
        return "false".into();
    }
    set.unwrap_or_else(|| "gh".into())
}

static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn cmd_log() -> Vec<String> {
    LOG.lock().map(|l| l.clone()).unwrap_or_default()
}

fn log_cmd(prog: &str, args: &[String]) {
    let mut v = vec![prog.to_string()];
    v.extend_from_slice(args);
    push_log(shell(&v));
}

/// An annotation under the command it belongs to in the `L` log.
pub fn log_note(line: &str) {
    push_log(format!("  {line}"));
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
            // real newlines inside single quotes are valid sh, so the line pastes back exactly
            format!("'{}'", a.replace('\'', "'\\''"))
        } else {
            a.clone()
        }
    };
    // shown to the user before anything runs: make look-alike / invisible characters visible
    crate::sanitize::clean(&args.iter().map(q).collect::<Vec<_>>().join(" ")).into_owned()
}

pub const LIMIT: usize = 100;

static SYNC_VIEWED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set once at startup from `sync_viewed` in the config.
pub fn set_sync_viewed(on: bool) {
    SYNC_VIEWED.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn sync_viewed() -> bool {
    SYNC_VIEWED.load(std::sync::atomic::Ordering::Relaxed)
}

const VIEWED_Q: &str = "query($o:String!,$n:String!,$p:Int!,$a:String){repository(owner:$o,name:$n){pullRequest(number:$p){id files(first:100,after:$a){pageInfo{hasNextPage endCursor} nodes{path viewerViewedState}}}} rateLimit{cost remaining resetAt limit}}";
const MARK_Q: &str = "mutation($id:ID!,$path:String!){markFileAsViewed(input:{pullRequestId:$id,path:$path}){clientMutationId}}";
const UNMARK_Q: &str = "mutation($id:ID!,$path:String!){unmarkFileAsViewed(input:{pullRequestId:$id,path:$path}){clientMutationId}}";

/// (PR node id, paths GitHub has as VIEWED, next cursor) from one page of `pullRequest.files`.
pub fn parse_viewed_page(graphql: &str) -> Result<(String, Vec<String>, Option<String>), String> {
    let v: Value = json(graphql)?;
    if let Some(e) = v["errors"][0]["message"].as_str() {
        return Err(e.to_string());
    }
    let pr = &v["data"]["repository"]["pullRequest"];
    let viewed = pr["files"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["viewerViewedState"] == "VIEWED")
        .filter_map(|f| f["path"].as_str().map(str::to_string))
        .collect();
    Ok((
        pr["id"].as_str().unwrap_or("").to_string(),
        viewed,
        next_cursor(&pr["files"]),
    ))
}

/// The PR's node id and the files GitHub has marked viewed (up to 3000 files).
fn fetch_viewed(repo: &str, n: &str) -> Result<(String, Vec<String>), String> {
    let (owner, name) = repo.split_once('/').ok_or("bad repo")?;
    let (mut id, mut all, mut after) = (String::new(), vec![], None::<String>);
    for _ in 0..30 {
        let mut a = vec![
            "api".to_string(),
            "graphql".into(),
            "-f".into(),
            format!("query={VIEWED_Q}"),
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
        let (pid, viewed, next) = parse_viewed_page(&graphql(a)?)?;
        (id, all) = (pid, [all, viewed].concat());
        match next {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    Ok((id, all))
}

/// The mutation as argv. Values travel as GraphQL variables (`-f`), never spliced into the query.
pub fn viewed_mutation(pr_id: &str, path: &str, viewed: bool) -> Vec<String> {
    let q = if viewed { MARK_Q } else { UNMARK_Q };
    ["gh", "api", "graphql", "-f"]
        .map(String::from)
        .into_iter()
        .chain([
            format!("query={q}"),
            "-f".into(),
            format!("id={pr_id}"),
            "-f".into(),
            format!("path={path}"),
        ])
        .collect()
}

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
    Tag,
    /// A row of the Repos panel (favorites / recent): `repo` is the name, the card text is in `body`.
    Repo,
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
    /// ISO timestamp of the last update (search results), for merging and sorting.
    #[serde(rename = "updatedAt")]
    pub updated: String,
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

static TIMEOUT_S: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(60);

/// `[api] timeout_s`: how long a read may take (pagination gets double).
pub fn set_timeout(secs: u64) {
    TIMEOUT_S.store(secs.max(1), std::sync::atomic::Ordering::Relaxed);
}

fn timeout() -> std::time::Duration {
    std::time::Duration::from_secs(TIMEOUT_S.load(std::sync::atomic::Ordering::Relaxed))
}

/// Processes we started and are still waiting for, so quitting can end them.
static CHILDREN: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Kills every `gh`/`git` child still running (called on the way out: no orphans).
pub fn kill_children() {
    let pids: Vec<String> = CHILDREN
        .lock()
        .map(|mut c| c.drain(..).map(|p| p.to_string()).collect())
        .unwrap_or_default();
    if !pids.is_empty() {
        let _ = Command::new("kill").arg("-KILL").args(&pids).output();
    }
}

/// Reads get `timeout_s`; paging through everything gets double.
fn limit_for(args: &[OsString]) -> std::time::Duration {
    if args.iter().any(|a| a == "--paginate") {
        timeout().saturating_mul(2)
    } else {
        timeout()
    }
}

/// Runs a command to completion within `limit`, collecting its output; on expiry the child is killed
/// and the error says so. `stdin` is piped in from a helper thread (large bodies).
fn run_child(
    mut c: Command,
    stdin: Option<&str>,
    limit: std::time::Duration,
) -> Result<std::process::Output, String> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    let prog = std::path::Path::new(c.get_program())
        .file_name()
        .map_or_else(|| "gh".into(), |n| n.to_string_lossy().into_owned());
    c.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("cannot run {prog}: {e}"))?;
    let pid = child.id();
    if let Ok(mut l) = CHILDREN.lock() {
        l.push(pid);
    }
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let text = text.to_string();
        // the child reads it all before answering; a broken pipe just means it failed early
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let reader = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut v = vec![];
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut v);
            }
            v
        })
    };
    let out = reader(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let err = reader(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Ok(st),
            Ok(None) if start.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("{prog} timed out after {}s", limit.as_secs()));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(e) => break Err(format!("cannot run {prog}: {e}")),
        }
    };
    if let Ok(mut l) = CHILDREN.lock() {
        l.retain(|p| *p != pid);
    }
    // after a kill the pipes may still be held by grandchildren: do not wait for the readers then
    let status = status?;
    Ok(std::process::Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

pub fn gh<I, S>(args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_gh(
        args.into_iter()
            .map(|a| a.as_ref().to_os_string())
            .collect(),
        &[],
    )
}

/// Every `gh` the app runs for itself goes through here: logged, rate-limit errors noted.
fn run_gh(args: Vec<OsString>, env: &[(&str, OsString)]) -> Result<String, String> {
    log_cmd(
        "gh",
        &args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    );
    let limit = limit_for(&args);
    let mut c = Command::new(program());
    c.args(args);
    c.envs(env.iter().map(|(k, v)| (k, v)));
    let o = run_child(c, None, limit).inspect_err(|e| push_log(format!("  error: {e}")))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        Err(throttle_note(
            crate::sanitize::clean(String::from_utf8_lossy(&o.stderr).trim()).into_owned(),
        ))
    }
}

/// A rate-limit error backs background work off, and says when the quota comes back.
fn throttle_note(e: String) -> String {
    if rate_limit_secs(&e).is_none() {
        return e;
    }
    crate::rate::note_error(&e);
    let st = crate::rate::snapshot();
    let now = crate::rate::now();
    let reset = [st.graphql, st.core]
        .into_iter()
        .flatten()
        .map(|b| b.reset)
        .filter(|r| *r > now)
        .min();
    match reset {
        Some(r) if !e.contains("resets") => format!("{e} (resets {})", crate::rate::clock(r)),
        _ => e,
    }
}

/// `gh api --cache <ttl>` for a slow-changing GET or query; `fresh` (the user pressed r/R) or
/// `[api] cache = false` runs it uncached. The cache files are gh's own (0600, no tokens), kept in
/// gh-pulse's cache directory so `--clear-cache` can remove exactly them.
pub fn gh_cached(args: Vec<String>, ttl: &str, fresh: bool) -> Result<String, String> {
    let (args, env) = cached_call(args, ttl, fresh);
    run_gh(args.into_iter().map(OsString::from).collect(), &env)
}

fn cached_call(
    mut args: Vec<String>,
    ttl: &str,
    fresh: bool,
) -> (Vec<String>, Vec<(&'static str, OsString)>) {
    let home = crate::cache::gh_home().filter(|_| crate::cache::enabled() && !fresh);
    match home {
        Some(h) if !args.is_empty() => {
            args.splice(1..1, ["--cache".to_string(), ttl.to_string()]);
            (args, vec![("XDG_CACHE_HOME", h.into_os_string())])
        }
        _ => (args, vec![]),
    }
}

/// Our own GraphQL calls: the `rateLimit` footer in the answer updates the shared quota state.
pub fn graphql(args: Vec<String>) -> Result<String, String> {
    let out = gh(args)?;
    crate::rate::note_graphql_response(&out);
    Ok(out)
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
    path: String,
}

const PR_FIELDS: &str = "number,title,url,state,isDraft,author,labels,body";
const ISSUE_FIELDS: &str = "number,title,url,state,author,labels,body";

#[derive(Deserialize, Default)]
#[serde(default)]
struct Notif {
    id: String,
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
                cmd: n.id, // thread id, for marking it read
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

/// Tag pages fetched (100 each); the Tags tab shows "300+" beyond that.
const TAG_PAGES: usize = 3;

/// Rows a source can return before its count is shown as "N+".
pub fn cap(panel: usize) -> usize {
    if panel == 7 { TAG_PAGES * LIMIT } else { LIMIT }
}

fn tag_rows(v: &Value, repo: &str) -> Vec<Item> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let name = t["name"].as_str()?;
            let sha = t["commit"]["sha"].as_str().unwrap_or("");
            Some(Item {
                title: name.into(),
                cmd: name.into(),
                meta: sha.chars().take(7).collect(),
                body: sha.into(),
                url: format!(
                    "https://github.com/{repo}/tree/{}",
                    crate::act::enc_path(name)
                ),
                repo: repo.into(),
                kind: Kind::Tag,
                ..Default::default()
            })
        })
        .collect()
}

fn tags(repo: &str, fresh: bool) -> Result<Vec<Item>, String> {
    let mut out = vec![];
    for page in 1..=TAG_PAGES {
        let path = format!("repos/{repo}/tags?per_page={LIMIT}&page={page}");
        let rows = tag_rows(
            &json(&gh_cached(vec!["api".into(), path], "10m", fresh)?)?,
            repo,
        );
        let full = rows.len() >= LIMIT;
        out.extend(rows);
        if !full {
            break;
        }
    }
    Ok(out)
}

/// Unread notifications across all repos (the API's default), for the inbox and its header badge.
pub fn notifications() -> Result<Vec<Item>, String> {
    let mut v = parse_notifications(&gh(["api", "notifications?per_page=100"])?)?;
    v.iter_mut().for_each(clean_item);
    Ok(v)
}

/// What the header and the empty detail pane say about the repo (text already neutralized).
#[derive(Default, Clone, serde::Serialize, Deserialize)]
pub struct RepoMeta {
    pub stars: u64,
    pub forks: u64,
    pub issues: u64,
    pub private: bool,
    pub branch: String,
    pub description: String,
    pub license: String,
    pub topics: Vec<String>,
    /// Day of the last push (YYYY-MM-DD).
    pub pushed: String,
}

impl RepoMeta {
    /// Neutralize text read back from disk, as everything from GitHub is on the way in.
    pub fn clean(&mut self) {
        use crate::sanitize::clean_in_place as c;
        c(&mut self.branch);
        c(&mut self.description);
        c(&mut self.license);
        c(&mut self.pushed);
        self.topics.iter_mut().for_each(c);
    }
}

/// `gh repo view`-shaped or GraphQL `repository` JSON -> the facts the header and overview show.
fn meta_from(v: &Value, topics: Vec<String>) -> RepoMeta {
    let s = |p: &str| {
        crate::sanitize::clean(v.pointer(p).and_then(Value::as_str).unwrap_or("")).into_owned()
    };
    RepoMeta {
        stars: v["stargazerCount"].as_u64().unwrap_or(0),
        forks: v["forkCount"].as_u64().unwrap_or(0),
        issues: v["openIssues"]["totalCount"]
            .as_u64()
            .or_else(|| v["issues"]["totalCount"].as_u64())
            .unwrap_or(0),
        private: v["isPrivate"] == true,
        branch: s("/defaultBranchRef/name"),
        description: s("/description"),
        license: s("/licenseInfo/name"),
        topics,
        pushed: day(&s("/pushedAt")).to_string(),
    }
}

/// What the first screen needs, asked for in one GraphQL request: the active tab of the PR and
/// Issues lists, the viewer, the repo's header facts and the quota. Values travel as variables.
const STARTUP_Q: &str = "\
query($o:String!,$n:String!,$qpr:String!,$qis:String!,$prSearch:Boolean!,$prOpen:Boolean!,$prMerged:Boolean!,$isSearch:Boolean!,$isOpen:Boolean!,$meta:Boolean!){\
 viewer{login}\
 rateLimit{cost remaining resetAt limit}\
 repository(owner:$o,name:$n){\
  openPrs:pullRequests(states:OPEN){totalCount}\
  ... on Repository @include(if:$meta){stargazerCount forkCount isPrivate description pushedAt licenseInfo{name} defaultBranchRef{name} openIssues:issues(states:OPEN){totalCount} repositoryTopics(first:20){nodes{topic{name}}}}\
  prOpen:pullRequests(states:OPEN,first:100,orderBy:{field:CREATED_AT,direction:DESC}) @include(if:$prOpen){nodes{...PRF}}\
  prMerged:pullRequests(states:MERGED,first:100,orderBy:{field:CREATED_AT,direction:DESC}) @include(if:$prMerged){nodes{...PRF}}\
  isOpen:issues(states:OPEN,first:100,orderBy:{field:CREATED_AT,direction:DESC}) @include(if:$isOpen){nodes{...ISF}}\
 }\
 prSearch:search(query:$qpr,type:ISSUE,first:100) @include(if:$prSearch){nodes{...PRF}}\
 isSearch:search(query:$qis,type:ISSUE,first:100) @include(if:$isSearch){nodes{...ISF}}\
}\
fragment PRF on PullRequest{number title url state isDraft body author{login} labels(first:20){nodes{name}}}\
fragment ISF on Issue{number title url state body author{login} labels(first:20){nodes{name}}}";

/// Which first-screen parts to fetch: the PR and issue tab ids (`gh::list` ids) to open with, and
/// whether the repo facts are needed (not when a fresh copy is cached).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct StartupWant {
    pub prs: Option<usize>,
    pub issues: Option<usize>,
    pub meta: bool,
}

pub struct Startup {
    pub prs: Option<Vec<Item>>,
    pub issues: Option<Vec<Item>>,
    pub meta: Option<RepoMeta>,
    pub user: String,
    pub open_prs: Option<usize>,
}

/// The variables of the startup request (all strings; booleans go as `-F`).
fn startup_vars(repo: &str, want: &StartupWant) -> Result<Vec<(String, String, bool)>, String> {
    let (o, n) = repo.split_once('/').ok_or("bad repo")?;
    let search =
        |kind: &str, qual: &str| format!("repo:{repo} is:{kind} is:open {qual} sort:created-desc");
    let qpr = match want.prs {
        Some(0) => search("pr", "author:@me"),
        Some(1) => search("pr", "review-requested:@me"),
        _ => String::new(),
    };
    let qis = match want.issues {
        Some(0) => search("issue", "assignee:@me"),
        Some(1) => search("issue", "author:@me"),
        _ => String::new(),
    };
    let b = |on: bool| (on.to_string(), true);
    let v = [
        ("o", (o.to_string(), false)),
        ("n", (n.to_string(), false)),
        ("qpr", (qpr, false)),
        ("qis", (qis, false)),
        ("prSearch", b(matches!(want.prs, Some(0 | 1)))),
        ("prOpen", b(want.prs == Some(2))),
        ("prMerged", b(want.prs == Some(3))),
        ("isSearch", b(matches!(want.issues, Some(0 | 1)))),
        ("isOpen", b(want.issues == Some(2))),
        ("meta", b(want.meta)),
    ];
    Ok(v.into_iter()
        .map(|(k, (v, raw))| (k.to_string(), v, raw))
        .collect())
}

/// One GraphQL call for the first screen.
pub fn startup(repo: &str, want: &StartupWant) -> Result<Startup, String> {
    let mut a: Vec<String> = ["api", "graphql", "-f"].map(String::from).to_vec();
    a.push(format!("query={STARTUP_Q}"));
    for (k, v, raw) in startup_vars(repo, want)? {
        a.extend([
            if raw { "-F" } else { "-f" }.to_string(),
            format!("{k}={v}"),
        ]);
    }
    parse_startup(&graphql(a)?, repo, want)
}

/// GraphQL nodes -> the `Item`s `gh pr list` / `gh issue list` would have produced.
fn graphql_items(nodes: &Value, kind: Kind, repo: &str) -> Vec<Item> {
    nodes
        .as_array()
        .into_iter()
        .flatten()
        .filter(|n| n["number"].is_u64())
        .filter_map(|n| {
            let mut n = n.clone();
            let labels: Vec<Value> = n["labels"]["nodes"].as_array().cloned().unwrap_or_default();
            n["labels"] = Value::Array(labels);
            if n["author"].is_null() {
                n.as_object_mut()?.remove("author");
            }
            let mut i: Item = serde_json::from_value(n).ok()?;
            (i.kind, i.repo) = (kind, repo.to_string());
            clean_item(&mut i);
            Some(i)
        })
        .collect()
}

pub fn parse_startup(body: &str, repo: &str, want: &StartupWant) -> Result<Startup, String> {
    let v: Value = json(body)?;
    if let Some(e) = v["errors"][0]["message"].as_str() {
        return Err(e.to_string());
    }
    let d = &v["data"];
    let r = &d["repository"];
    let pick = |a: &str, b: &str| {
        let n = if r[a].is_object() { &r[a] } else { &d[b] };
        n["nodes"].clone()
    };
    let prs = want.prs.map(|t| {
        let nodes = match t {
            0 | 1 => pick("none", "prSearch"),
            2 => pick("prOpen", "none"),
            _ => pick("prMerged", "none"),
        };
        graphql_items(&nodes, Kind::Pr, repo)
    });
    let issues = want.issues.map(|t| {
        let nodes = match t {
            0 | 1 => pick("none", "isSearch"),
            _ => pick("isOpen", "none"),
        };
        graphql_items(&nodes, Kind::Issue, repo)
    });
    let topics = r["repositoryTopics"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["topic"]["name"].as_str())
        .map(|t| crate::sanitize::clean(t).into_owned())
        .collect();
    Ok(Startup {
        prs,
        issues,
        meta: want.meta.then(|| meta_from(r, topics)),
        user: crate::sanitize::clean(d["viewer"]["login"].as_str().unwrap_or("")).into_owned(),
        open_prs: r["openPrs"]["totalCount"].as_u64().map(|n| n as usize),
    })
}

/// `owner/name` of the first GitHub remote in `git remote -v` output (`origin` first). Offline: no API.
pub fn repo_from_remotes(remotes: &str, host: &str) -> Option<String> {
    let parse = |url: &str| {
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .or_else(|| url.strip_prefix("ssh://"))
            .unwrap_or(url);
        let rest = rest.split_once('@').map_or(rest, |(_, r)| r); // user@host
        let path = rest
            .strip_prefix(host)?
            .trim_start_matches([':', '/'])
            .trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        crate::state::valid_repo(path).then(|| path.to_string())
    };
    let mut fetch: Vec<(&str, &str)> = remotes
        .lines()
        .filter(|l| l.ends_with("(fetch)"))
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            Some((w.next()?, w.next()?))
        })
        .collect();
    fetch.sort_by_key(|(name, _)| *name != "origin");
    fetch.into_iter().find_map(|(_, url)| parse(url))
}

/// The cwd's repo judged from its git remotes (no API call), if it has a GitHub one.
pub fn local_repo() -> Option<String> {
    let o = Command::new("git").args(["remote", "-v"]).output().ok()?;
    o.status
        .success()
        .then(|| repo_from_remotes(&String::from_utf8_lossy(&o.stdout), &host()))?
}

/// The cwd is inside a git work tree at all (so asking GitHub which repo it is can make sense).
pub fn in_git_repo() -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The GitHub host `gh` talks to by default.
pub fn host() -> String {
    std::env::var("GH_HOST")
        .ok()
        .filter(|h| {
            !h.is_empty()
                && h.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._:".contains(c))
        })
        .unwrap_or_else(|| "github.com".into())
}

/// The active login for `host` as gh's own `hosts.yml` records it: a local read, no API call, and
/// never the token. None when it isn't recorded (a token from the environment, say).
pub fn login_in(hosts_yml: &str, host: &str) -> Option<String> {
    let mut in_host = false;
    for l in hosts_yml.lines() {
        if !l.starts_with([' ', '\t']) {
            in_host = l.trim_end().strip_suffix(':') == Some(host);
        } else if in_host && let Some(u) = l.trim().strip_prefix("user:") {
            let u = u.trim().trim_matches(['"', '\'']);
            let ok = !u.is_empty()
                && u.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
            return ok.then(|| u.to_string());
        }
    }
    None
}

#[cfg(test)]
static IDENTITY: Mutex<Option<Option<(String, String)>>> = Mutex::new(None);

#[cfg(test)]
pub fn clear_identity() {
    *IDENTITY.lock().unwrap() = None;
}

#[cfg(test)]
pub fn set_identity(id: Option<(String, String)>) {
    *IDENTITY.lock().unwrap() = Some(id);
}

/// (host, login) that on-disk caches are keyed by; None means nothing user-specific is cached.
pub fn identity() -> Option<(String, String)> {
    #[cfg(test)]
    if let Some(id) = IDENTITY.lock().unwrap().clone() {
        return id;
    }
    static ID: std::sync::OnceLock<Option<(String, String)>> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let dir = std::env::var_os("GH_CONFIG_DIR")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_CONFIG_HOME")
                    .filter(|v| !v.is_empty())
                    .map(|x| std::path::Path::new(&x).join("gh"))
            })
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config/gh"))
            })?;
        let host = host();
        let login = login_in(&std::fs::read_to_string(dir.join("hosts.yml")).ok()?, &host)?;
        Some((host, login))
    })
    .clone()
}

/// Repo facts alone (the fallback when the batched request failed).
pub fn repo_meta(repo: &str) -> Result<RepoMeta, String> {
    let f = "stargazerCount,forkCount,issues,isPrivate,defaultBranchRef,description,licenseInfo,repositoryTopics,pushedAt";
    let v: Value = json(&gh(["repo", "view", repo, "--json", f])?)?;
    let topics = v["repositoryTopics"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["name"].as_str())
        .map(|t| crate::sanitize::clean(t).into_owned())
        .collect();
    Ok(meta_from(&v, topics))
}

/// Whether the working directory is a clone of `repo`, judged from its git remotes (no API call).
pub fn cwd_is_clone_of(repo: &str) -> bool {
    Command::new("git")
        .args(["remote", "-v"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| remotes_include(&String::from_utf8_lossy(&o.stdout), repo))
}

fn remotes_include(remotes: &str, repo: &str) -> bool {
    let want = repo.to_lowercase();
    remotes.lines().any(|l| {
        let url = l.split_whitespace().nth(1).unwrap_or("").to_lowercase();
        let url = url.trim_end_matches(".git");
        url.ends_with(&format!("github.com/{want}")) || url.ends_with(&format!("github.com:{want}"))
    })
}

/// (rows, more than that) of one list tab, for the panel titles.
pub fn count(repo: &str, panel: usize, tab: usize) -> Result<(usize, bool), String> {
    let n = list(repo, panel, tab, false)?.len();
    Ok((n, n >= cap(panel)))
}

/// Panels: 0 Status, 1 Pull requests, 2 Issues, 3 Actions, 4 Branches, 5 Releases, 6 Notifications, 7 Tags.
/// In the global view panels 1-2 search across all repos and the repo-only ones are empty.
/// `fresh` skips the on-disk cache (the user asked for a refresh).
pub fn list(repo: &str, panel: usize, tab: usize, fresh: bool) -> Result<Vec<Item>, String> {
    let mut v = list_raw(repo, panel, tab, fresh)?;
    v.iter_mut().for_each(clean_item);
    Ok(v)
}

fn list_raw(repo: &str, panel: usize, tab: usize, fresh: bool) -> Result<Vec<Item>, String> {
    let limit = format!("--limit={LIMIT}");
    if panel == 6 {
        let all = parse_notifications(&gh(["api", "notifications"])?)?;
        // Scoped to the current repo unless in the global view.
        return Ok(all
            .into_iter()
            .filter(|n| n.repo.eq_ignore_ascii_case(repo))
            .collect());
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
            let out = gh([
                "workflow",
                "list",
                "-R",
                repo,
                "--json",
                "id,name,state,path",
            ])?;
            Ok(json::<Vec<Workflow>>(&out)?
                .into_iter()
                .map(|w| Item {
                    number: w.id,
                    title: w.name,
                    state: w.state,
                    cmd: w.path, // .github/workflows/x.yml, to read its dispatch inputs
                    url: format!("https://github.com/{repo}/actions"),
                    repo: repo.into(),
                    kind: Kind::Workflow,
                    ..Default::default()
                })
                .collect())
        }
        4 => branches(repo),
        7 => tags(repo, fresh),
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
    /// GraphQL node id, to ask for who reacted.
    pub id: String,
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

#[derive(Default)]
pub struct FilesData {
    /// Head sha; anchors inline comments.
    pub sha: String,
    pub files: Vec<diff::File>,
    /// Review threads per path.
    pub threads: std::collections::HashMap<String, u32>,
    /// With `sync_viewed`: the PR's GraphQL node id, the key of its local viewed marks, and the paths
    /// GitHub already has marked viewed for you.
    pub pr_id: String,
    pub viewed_key: String,
    pub gh_viewed: Vec<String>,
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

/// `reactionGroups` of a comment: counts always, names only when the query asked for them.
fn parse_reactions(groups: &Value) -> Vec<Reaction> {
    groups
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
        .collect()
}

const REACTORS_Q: &str = "query($id:ID!){node(id:$id){... on Reactable{reactionGroups{content users(first:25){totalCount nodes{login}}}}} rateLimit{cost remaining resetAt limit}}";

/// Who reacted to one comment (the on-demand call behind `e`; the first page only has counts).
pub fn reactors(id: &str) -> Result<Vec<Reaction>, String> {
    let out = graphql(vec![
        "api".to_string(),
        "graphql".into(),
        "-f".into(),
        format!("query={REACTORS_Q}"),
        "-f".into(),
        format!("id={id}"),
    ])?;
    parse_reactors(&out)
}

pub fn parse_reactors(body: &str) -> Result<Vec<Reaction>, String> {
    let v: Value = json(body)?;
    if let Some(e) = v["errors"][0]["message"].as_str() {
        return Err(e.to_string());
    }
    let mut r = parse_reactions(&v["data"]["node"]["reactionGroups"]);
    r.iter_mut()
        .for_each(|x| x.users.iter_mut().for_each(crate::sanitize::clean_in_place));
    Ok(r)
}

impl Card {
    /// Needs a names lookup: some reaction has a count but no names yet.
    pub fn missing_names(&self) -> bool {
        !self.id.is_empty() && self.reactions.iter().any(|r| r.users.is_empty())
    }

    /// Ids of this card and its replies that still need their names.
    pub fn nameless_ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .missing_names()
            .then(|| self.id.clone())
            .into_iter()
            .collect();
        v.extend(self.replies.iter().flat_map(Card::nameless_ids));
        v
    }

    /// Fills in names for the card (or reply) with this id; false when none matches.
    pub fn set_reactors(&mut self, id: &str, names: &[Reaction]) -> bool {
        if self.id == id {
            for r in &mut self.reactions {
                if let Some(n) = names.iter().find(|n| n.content == r.content) {
                    r.users = n.users.clone();
                }
            }
            return true;
        }
        self.replies.iter_mut().any(|c| c.set_reactors(id, names))
    }
}

fn card_from(n: &Value) -> Card {
    let s = |k: &str| n[k].as_str().unwrap_or("").to_string();
    let reactions = parse_reactions(&n["reactionGroups"]);
    Card {
        id: s("id"),
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

// Reactions come as counts only (names are one call away, on `e`); `id` is for that call.
const CARD_FIELDS: &str = "id author{login} authorAssociation lastEditedAt body reactionGroups{content users{totalCount}}";

const PAGE: &str = "totalCount pageInfo{hasNextPage endCursor}";

/// One connection's selection. `after` pages it; the first request omits it.
fn conn_query(name: &str, after: bool) -> String {
    let c = CARD_FIELDS;
    let a = if after { ",after:$a" } else { "" };
    // the first request is the one that must be quick and cheap: 50 per connection, 20 replies a thread
    let n = if after { 100 } else { FIRST_PAGE };
    match name {
        "comments" => format!("comments(first:{n}{a}){{{PAGE} nodes{{createdAt {c}}}}}"),
        "reviews" => format!("reviews(first:{n}{a}){{{PAGE} nodes{{state submittedAt {c}}}}}"),
        _ => format!(
            "reviewThreads(first:{n}{a}){{{PAGE} nodes{{id isResolved isOutdated path line comments(first:{r}){{{PAGE} nodes{{createdAt {c}}}}}}}}}",
            r = if after { 50 } else { FIRST_REPLIES }
        ),
    }
}

/// Items per connection and replies per thread in the first comments request.
const FIRST_PAGE: usize = 50;
const FIRST_REPLIES: usize = 20;

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
    format!(
        "query($o:String!,$n:String!,$p:Int!{a}){{repository(owner:$o,name:$n){{{body}}} rateLimit{{cost remaining resetAt limit}}}}"
    )
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
    graphql(a)
}

fn fetch_comments(repo: &str, n: &str, pr: bool) -> Result<CommentsData, String> {
    let mut cd = parse_comments(&graphql_comments(repo, n, pr, None, None)?, pr)?;
    cd.loading = !cd.pend.is_empty();
    Ok(cd)
}

const REPLIES_Q: &str = "query($id:ID!,$a:String){node(id:$id){... on PullRequestReviewThread{comments(first:50,after:$a){totalCount pageInfo{hasNextPage endCursor} nodes{createdAt CARD}}}} rateLimit{cost remaining resetAt limit}}";

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
            .min(MAX_BACKOFF_SECS)
    })
}

/// Longest wait any server message can make us accept (a hostile `Retry-After: 99999999999` included).
pub const MAX_BACKOFF_SECS: u64 = 3600;

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
                let out = graphql(vec![
                    "api".to_string(),
                    "graphql".into(),
                    "-f".into(),
                    format!("query={q}"),
                    "-f".into(),
                    format!("id={id}"),
                    "-f".into(),
                    format!("a={c}"),
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
            Kind::Issue | Kind::Workflow | Kind::Branch | Kind::Repo | Kind::Other,
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
            let mut c = Command::new(program());
            c.args(a);
            let o = run_child(c, None, timeout())?;
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
            // opted-in: also learn the PR node id and what GitHub has marked viewed (best effort)
            let (pr_id, gh_viewed) = if sync_viewed() {
                fetch_viewed(repo, &n).unwrap_or_default()
            } else {
                Default::default()
            };
            Ok(Data::Files(FilesData {
                sha: sha.trim().into(),
                files: diff::parse(&d),
                threads: counts,
                pr_id,
                viewed_key: crate::state::Viewed::key(repo, it.number),
                gh_viewed,
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
                ..Default::default()
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
        (Kind::Tag, Tab::Overview) => tag_info(repo, it),
        (Kind::Status, _) => status(repo).map(text),
        _ => Ok(Data::Text(vec![])),
    }
}

/// Commit, date and the release (if one exists) behind a tag.
fn tag_info(repo: &str, it: &Item) -> Result<Data, String> {
    let (tag, sha) = (it.cmd_name(), it.body.as_str());
    if sha.is_empty() {
        return Err("this tag has no commit sha".into());
    }
    let c: Value = json(&gh(["api", &format!("repos/{repo}/commits/{sha}")])?)?;
    let s = |p: &str| {
        c.pointer(p)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let release = match gh([
        "api",
        &format!("repos/{repo}/releases/tags/{}", crate::act::enc_path(tag)),
    ]) {
        Ok(r) => {
            let r: Value = json(&r)?;
            r["html_url"].as_str().unwrap_or("").to_string()
        }
        Err(e) if e.contains("Not Found") || e.contains("404") => "none".into(),
        Err(e) => format!("unknown ({e})"),
    };
    Ok(Data::Text(vec![
        format!("tag:      {tag}"),
        format!("commit:   {sha}"),
        format!("date:     {}", s("/commit/committer/date")),
        format!("author:   {}", s("/commit/author/name")),
        format!(
            "message:  {}",
            s("/commit/message").lines().next().unwrap_or("")
        ),
        format!("release:  {release}"),
    ]))
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

const COMMITS_Q: &str = "query($o:String!,$n:String!,$p:Int!,$a:String){repository(owner:$o,name:$n){pullRequest(number:$p){commits(first:100,after:$a){pageInfo{hasNextPage endCursor} nodes{commit{oid messageHeadline committedDate additions deletions author{name user{login}}}}}}} rateLimit{cost remaining resetAt limit}}";

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
        let (rows, next) = parse_commits(&graphql(a)?)?;
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
    let q = "query($o:String!,$n:String!,$p:Int!){repository(owner:$o,name:$n){pullRequest(number:$p){reviewThreads(first:100){nodes{path}}}} rateLimit{cost remaining resetAt limit}}";
    let out = graphql(vec![
        "api".to_string(),
        "graphql".into(),
        "-f".into(),
        format!("query={q}"),
        "-f".into(),
        format!("o={owner}"),
        "-f".into(),
        format!("n={name}"),
        "-F".into(),
        format!("p={n}"),
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
/// Like `run`, optionally piping `stdin` to the command (large bodies via `--body-file -`).
pub fn run_with(cmd: &[String], stdin: Option<&str>) -> Result<String, String> {
    let Some((prog, args)) = cmd.split_first() else {
        return Err("empty command".into());
    };
    log_cmd(prog, args);
    let exe = if prog == "gh" { program() } else { prog.into() };
    let mut c = Command::new(exe);
    c.args(args);
    // a write may legitimately take long (an upload): five times the read limit
    let o = run_child(c, stdin, timeout().saturating_mul(5)).map_err(|e| {
        if e.contains("timed out") {
            push_log(format!("  error: {e}"));
        }
        e
    })?;
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

/// Everything the create-issue / create-PR / run-workflow forms look up in the background (each part
/// optional). Names and bodies are the real ones, because they end up in the command; the popups
/// neutralize them for display only.
#[derive(Default)]
pub struct FormData {
    pub labels: Vec<String>,
    pub templates: Vec<(String, String)>,
    pub default_branch: String,
    pub branches: Vec<String>,
    pub tags: Vec<String>,
    pub head_pushed: Option<bool>,
    pub pr_template: Option<String>,
    /// The workflow's dispatch inputs, parsed here in the background thread (None: file not fetched).
    pub dispatch: Option<Result<Option<Vec<crate::dispatch::InputDef>>, String>>,
}

/// "---\nname: Bug\n---\nbody" -> ("Bug", "body"); no front matter -> (None, whole text).
pub fn split_front_matter(t: &str) -> (Option<String>, String) {
    let Some(rest) = t.strip_prefix("---\n") else {
        return (None, t.to_string());
    };
    let Some((head, body)) = rest
        .split_once("\n---\n")
        .or_else(|| rest.split_once("\n---\r\n"))
    else {
        return (None, t.to_string());
    };
    let name = head
        .lines()
        .find_map(|l| l.strip_prefix("name:"))
        .map(|n| n.trim().trim_matches(['"', '\'']).to_string());
    (
        name.filter(|n| !n.is_empty()),
        body.trim_start_matches('\n').to_string(),
    )
}

/// (display name, path) of the Markdown files in a contents-API directory listing.
pub fn parse_template_list(json_text: &str) -> Vec<(String, String)> {
    let v: Value = serde_json::from_str(json_text).unwrap_or_default();
    v.as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["type"] == "file")
        .filter_map(|f| {
            let (name, path) = (f["name"].as_str()?, f["path"].as_str()?);
            let stem = name.strip_suffix(".md")?; // YAML issue forms can't prefill a body
            Some((stem.replace(['_', '-'], " "), path.to_string()))
        })
        .collect()
}

/// A file from the default branch via the contents API. Path segments are percent-encoded and `..` is refused.
fn raw_file(repo: &str, path: &str) -> Option<String> {
    if path.split('/').any(|seg| seg == ".." || seg.is_empty()) {
        return None;
    }
    gh_cached(
        vec![
            "api".into(),
            "-H".into(),
            "Accept: application/vnd.github.raw".into(),
            format!("repos/{repo}/contents/{}", crate::act::enc_path(path)),
        ],
        "1h",
        false,
    )
    .ok()
}

/// Names, one per line (--paginate output), at most `cap` of them.
fn name_lines(s: String, cap: usize) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(cap)
        .map(String::from)
        .collect()
}

fn default_branch(repo: &str) -> String {
    gh([
        "repo",
        "view",
        repo,
        "--json",
        "defaultBranchRef",
        "-q",
        ".defaultBranchRef.name",
    ])
    .map(|s| s.trim().to_string())
    .unwrap_or_default()
}

fn branch_names(repo: &str) -> Vec<String> {
    let path = format!("repos/{repo}/branches?per_page=100");
    gh(["api", &path, "--paginate", "--jq", ".[].name"])
        .map(|s| name_lines(s, 1000))
        .unwrap_or_default()
}

/// Best effort: a failed lookup just leaves that part empty. `head` is set for the PR form.
pub fn form_data(repo: &str, head: Option<&str>, workflow_path: Option<&str>) -> FormData {
    let mut d = FormData::default();
    if let Some(path) = workflow_path {
        d.default_branch = default_branch(repo);
        d.branches = branch_names(repo);
        d.tags = gh_cached(
            vec![
                "api".into(),
                format!("repos/{repo}/tags?per_page=100"),
                "--jq".into(),
                ".[].name".into(),
            ],
            "10m",
            false,
        )
        .map(|s| name_lines(s, 100))
        .unwrap_or_default();
        // parsed here, off the UI thread, with the size and alias limits of `dispatch::parse`
        d.dispatch = raw_file(repo, path).map(|y| crate::dispatch::parse(&y));
        return d;
    }
    // up to 300 names are kept: a repo with more skips label validation (see Form::set_labels)
    d.labels = gh_cached(
        vec![
            "api".into(),
            format!("repos/{repo}/labels?per_page=100"),
            "--paginate".into(),
            "--jq".into(),
            ".[].name".into(),
        ],
        "1h",
        false,
    )
    .map(|s| name_lines(s, 300))
    .unwrap_or_default();
    match head {
        None => {
            if let Ok(list) = gh_cached(
                vec![
                    "api".into(),
                    format!("repos/{repo}/contents/.github/ISSUE_TEMPLATE"),
                ],
                "1h",
                false,
            ) {
                for (name, path) in parse_template_list(&list).into_iter().take(10) {
                    if let Some(text) = raw_file(repo, &path) {
                        let (fm_name, body) = split_front_matter(&text);
                        d.templates.push((fm_name.unwrap_or(name), body));
                    }
                }
            }
        }
        Some(h) => {
            d.default_branch = default_branch(repo);
            d.branches = branch_names(repo);
            d.head_pushed = match gh([
                "api",
                &format!("repos/{repo}/branches/{}", crate::act::enc_path(h)),
                "--jq",
                ".name",
            ]) {
                Ok(_) => Some(true),
                Err(e) if e.contains("Not Found") || e.contains("404") => Some(false),
                Err(_) => None,
            };
            d.pr_template = [
                ".github/pull_request_template.md",
                ".github/PULL_REQUEST_TEMPLATE.md",
                "docs/pull_request_template.md",
                "pull_request_template.md",
            ]
            .iter()
            .find_map(|p| raw_file(repo, p));
        }
    }
    d
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
    .map_err(|e| resolve_error(repo, &e))
}

fn resolve_error(repo: &str, stderr: &str) -> String {
    match rate_limit_secs(stderr) {
        Some(s) => format!("GitHub rate limit exceeded; retry in ~{s}s"),
        None => format!("repo not found or no access: {repo}"),
    }
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
    #[test]
    fn the_local_repo_comes_from_git_remotes_without_an_api_call() {
        let r = |s: &str| repo_from_remotes(s, "github.com");
        assert_eq!(
            r("origin\thttps://github.com/cli/cli.git (fetch)\norigin\thttps://github.com/cli/cli.git (push)\n"),
            Some("cli/cli".into())
        );
        assert_eq!(r("origin\tgit@github.com:o/r.git (fetch)\n"), Some("o/r".into()));
        assert_eq!(r("origin\tssh://git@github.com/o/r (fetch)\n"), Some("o/r".into()));
        assert_eq!(
            r("up\thttps://github.com/up/stream (fetch)\norigin\thttps://github.com/me/fork (fetch)\n"),
            Some("me/fork".into()),
            "origin first"
        );
        assert_eq!(r("origin\thttps://gitlab.com/o/r (fetch)\n"), None);
        assert_eq!(r("origin\thttps://github.com/o/r/extra (fetch)\n"), None);
        assert_eq!(r(""), None);
        assert_eq!(repo_from_remotes("origin\thttps://ghe.corp/o/r (fetch)\n", "ghe.corp"), Some("o/r".into()));
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_gh_is_killed_at_the_deadline_and_on_quit() {
        let shim = crate::testshim::Shim::new();
        shim.set("sleep", "30");
        set_timeout(1);
        let t = std::time::Instant::now();
        let e = gh(["api", "x"]).unwrap_err();
        assert!(e.contains("timed out after 1s"), "{e}");
        assert!(
            t.elapsed().as_secs() < 25,
            "killed at the deadline, not after the 30 s sleep"
        );
        // the killed child really is gone (not left sleeping)
        std::thread::sleep(std::time::Duration::from_millis(200));
        let alive = Command::new("pgrep")
            .args(["-f", "sleep 30"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&alive.stdout).trim().is_empty(),
            "orphan left behind"
        );
        assert!(cmd_log().iter().any(|l| l.contains("error: gh timed out")));
        // pagination gets double, writes five times
        let a = |s: &str| vec![OsString::from(s)];
        assert_eq!(limit_for(&a("api")).as_secs(), 1);
        assert_eq!(limit_for(&a("--paginate")).as_secs(), 2);
        // quitting ends whatever is still running
        set_timeout(60);
        let h = std::thread::spawn(|| gh(["api", "y"]));
        for _ in 0..2000 {
            if !CHILDREN.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let t = std::time::Instant::now();
        kill_children();
        assert!(h.join().unwrap().is_err());
        assert!(t.elapsed().as_secs() < 25, "no 30 s wait for an orphan");
    }

    #[test]
    fn the_batched_startup_response_gives_the_same_items_as_the_cli_lists() {
        let want = StartupWant {
            prs: Some(0),
            issues: Some(0),
            meta: true,
        };
        let st = parse_startup(include_str!("../tests/startup.json"), "o/r", &want).unwrap();
        let old = parse_items(include_str!("../tests/startup_cli.json"), Kind::Pr, "o/r").unwrap();
        let new = st.prs.unwrap();
        assert_eq!(
            new.len(),
            2,
            "empty search nodes (issues among PRs) are skipped"
        );
        for (a, b) in new.iter().zip(&old) {
            assert_eq!(
                (
                    a.number,
                    &a.title,
                    &a.url,
                    &a.state,
                    a.is_draft,
                    &a.body,
                    &a.author.login,
                    a.kind,
                    &a.repo
                ),
                (
                    b.number,
                    &b.title,
                    &b.url,
                    &b.state,
                    b.is_draft,
                    &b.body,
                    &b.author.login,
                    b.kind,
                    &b.repo
                )
            );
            assert_eq!(
                a.labels.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
                b.labels.iter().map(|l| l.name.as_str()).collect::<Vec<_>>()
            );
        }
        let is = st.issues.unwrap();
        assert_eq!(
            (is[0].number, is[0].kind, is[0].author.login.as_str()),
            (3, Kind::Issue, "bob")
        );
        let m = st.meta.unwrap();
        assert_eq!(
            (
                m.stars,
                m.forks,
                m.issues,
                m.branch.as_str(),
                m.license.as_str(),
                m.topics.len()
            ),
            (1234, 56, 38, "trunk", "MIT License", 2)
        );
        assert_eq!(
            (st.user.as_str(), st.open_prs, m.pushed.as_str()),
            ("octocat", Some(74), "2026-10-02")
        );
        // text from GitHub is neutralized like every other list
        let evil = include_str!("../tests/startup.json").replace("Add notes", "Add\\u202e notes");
        let st = parse_startup(&evil, "o/r", &want).unwrap();
        assert!(st.prs.unwrap()[0].title.contains("<U+202E>"));
        // without the facts (cached) none are claimed
        let none = StartupWant {
            meta: false,
            ..want
        };
        assert!(
            parse_startup(include_str!("../tests/startup.json"), "o/r", &none)
                .unwrap()
                .meta
                .is_none()
        );
    }

    #[test]
    fn startup_variables_carry_the_values_and_the_query_is_constant() {
        let v = |w: &StartupWant| startup_vars("o/r", w).unwrap();
        let get = |v: &[(String, String, bool)], k: &str| {
            v.iter().find(|x| x.0 == k).map(|x| x.1.clone()).unwrap()
        };
        let mine = v(&StartupWant {
            prs: Some(0),
            issues: Some(2),
            meta: false,
        });
        assert_eq!(
            get(&mine, "qpr"),
            "repo:o/r is:pr is:open author:@me sort:created-desc"
        );
        assert_eq!(get(&mine, "prSearch"), "true");
        assert_eq!(get(&mine, "isOpen"), "true");
        assert_eq!(get(&mine, "meta"), "false");
        let merged = v(&StartupWant {
            prs: Some(3),
            issues: Some(1),
            meta: true,
        });
        assert_eq!(get(&merged, "prMerged"), "true");
        assert_eq!(
            get(&merged, "qis"),
            "repo:o/r is:issue is:open author:@me sort:created-desc"
        );
        assert!(
            !STARTUP_Q.contains("o/r")
                && STARTUP_Q.contains("rateLimit{cost remaining resetAt limit}")
        );
        assert!(startup_vars("nonsense", &StartupWant::default()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn one_graphql_call_covers_startup_and_feeds_the_quota_state() {
        let shim = crate::testshim::Shim::new();
        shim.set("graphql.out", include_str!("../tests/startup.json"));
        let want = StartupWant {
            prs: Some(0),
            issues: Some(0),
            meta: true,
        };
        let st = startup("o/r", &want).unwrap();
        assert_eq!(st.prs.unwrap().len(), 2);
        let calls = shim.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(
            calls[0].starts_with("api graphql -f query=query($o:String!")
                && calls[0].contains("-f o=o -f n=r"),
            "{calls:?}"
        );
        assert!(
            calls[0].contains("-F meta=true"),
            "booleans travel as variables: {calls:?}"
        );
        let g = crate::rate::snapshot()
            .graphql
            .expect("rateLimit footer was noted");
        assert_eq!((g.remaining, g.limit), (4990, 5000));
        assert!(
            cmd_log()
                .iter()
                .any(|l| l.contains("rate: cost 1, graphql 4990/5000 left"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn cached_calls_add_the_flag_only_when_allowed() {
        // the cache switch is process-wide: hold the shim lock so no other test sees it flip
        let _shim = crate::testshim::Shim::new();
        crate::cache::set_enabled(true);
        let base = || vec!["api".to_string(), "repos/o/r/tags".to_string()];
        let (a, env) = cached_call(base(), "10m", false);
        if crate::cache::gh_home().is_some() {
            assert_eq!(a, ["api", "--cache", "10m", "repos/o/r/tags"]);
            assert_eq!(env[0].0, "XDG_CACHE_HOME");
        }
        let (a, env) = cached_call(base(), "10m", true);
        assert_eq!(a, base(), "r/R bypass the cache");
        assert!(env.is_empty());
        crate::cache::set_enabled(false);
        let (a, _) = cached_call(base(), "10m", false);
        crate::cache::set_enabled(true);
        assert_eq!(a, base(), "[api] cache = false");
    }

    #[cfg(unix)]
    #[test]
    fn slow_lookups_are_cached_privately_and_refresh_skips_the_cache() {
        let shim = crate::testshim::Shim::new();
        shim.set("rest.out", r#"[{"name":"v1","commit":{"sha":"abc1234"}}]"#);
        list("o/r", 7, 0, false).unwrap();
        list("o/r", 7, 0, true).unwrap();
        form_data("o/r", None, None);
        let calls = shim.calls();
        assert!(
            calls[0].starts_with("api --cache 10m repos/o/r/tags"),
            "{calls:?}"
        );
        assert!(!calls[1].contains("--cache"), "fresh: {calls:?}");
        assert!(
            calls
                .iter()
                .any(|c| c.contains("--cache 1h") && c.contains("labels")),
            "labels cached an hour: {calls:?}"
        );
        let envs = shim.envs();
        assert!(
            envs[0].ends_with("cache"),
            "gh's cache lives in gh-pulse's directory: {envs:?}"
        );
        assert_eq!(envs[1], "", "uncached calls leave the environment alone");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&envs[0]).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "the cache directory is private");
        // writes and comments never ask for the cache
        assert!(!viewed_mutation("id", "p", true).contains(&"--cache".to_string()));
        assert!(!comments_query(true, None).contains("cache"));
    }

    #[test]
    fn tags_and_notifications_carry_what_the_actions_need() {
        let v: Value = serde_json::from_str(
            r#"[{"name":"v1.2.0","commit":{"sha":"abcdef0123456789"}},{"name":"-x","commit":{"sha":"1"}},{"nope":1}]"#,
        )
        .unwrap();
        let t = tag_rows(&v, "o/r");
        assert_eq!(t.len(), 2);
        assert_eq!(
            (
                t[0].kind,
                t[0].cmd_name(),
                t[0].meta.as_str(),
                t[0].body.as_str()
            ),
            (Kind::Tag, "v1.2.0", "abcdef0", "abcdef0123456789")
        );
        assert_eq!(t[0].url, "https://github.com/o/r/tree/v1.2.0");
        assert_eq!(cap(7), 300);
        assert_eq!(cap(1), LIMIT);
        let n = parse_notifications(
            r#"[{"id":"1001","unread":true,"reason":"mention","subject":{"title":"T","url":"https://api.github.com/repos/o/r/pulls/7","type":"PullRequest"},"repository":{"full_name":"o/r"}}]"#,
        )
        .unwrap();
        assert_eq!(
            (n[0].cmd.as_str(), n[0].number, n[0].kind),
            ("1001", 7, Kind::Pr)
        );
        assert_eq!(
            shell(&crate::act::mark_read("1001")),
            "gh api -X PATCH notifications/threads/1001"
        );
    }

    #[test]
    fn shell_line_round_trips_through_sh() {
        let args: Vec<String> = [
            "a b",
            "it's",
            "two\nlines\n",
            "back\\slash $HOME `x`",
            "",
            "plain",
        ]
        .map(String::from)
        .to_vec();
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf '%s\\0' {}", shell(&args)))
            .output()
            .unwrap();
        let got: Vec<&str> = std::str::from_utf8(&out.stdout)
            .unwrap()
            .split_terminator('\0')
            .collect();
        assert_eq!(got, args);
    }

    #[test]
    fn resolve_errors_tell_rate_limits_from_missing_repos() {
        for e in [
            "GraphQL: API rate limit already exceeded for user ID 1234.",
            "HTTP 403: API rate limit exceeded for user ID 1234. (https://api.github.com/x)",
            "HTTP 403: You have exceeded a secondary rate limit. Retry-After: 45",
        ] {
            assert!(
                resolve_error("o/r", e).starts_with("GitHub rate limit exceeded; retry in ~"),
                "{e}"
            );
        }
        assert!(resolve_error("o/r", "x Retry-After: 45").contains("~45s"));
        assert_eq!(
            resolve_error(
                "o/r",
                "GraphQL: Could not resolve to a Repository with the name 'o/r'."
            ),
            "repo not found or no access: o/r"
        );
    }

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
    fn issue_templates_front_matter_and_listing() {
        let t = "---\nname: \"Bug report\"\nabout: Something broke\nlabels: bug\n---\n### Steps\n1. do\n";
        let (name, body) = split_front_matter(t);
        assert_eq!(name.as_deref(), Some("Bug report"));
        assert_eq!(body, "### Steps\n1. do\n");
        assert_eq!(
            split_front_matter("no front matter"),
            (None, "no front matter".to_string())
        );
        let list = r#"[{"name":"bug_report.md","path":".github/ISSUE_TEMPLATE/bug_report.md","type":"file"},
                       {"name":"config.yml","path":".github/ISSUE_TEMPLATE/config.yml","type":"file"},
                       {"name":"form.yml","path":".github/ISSUE_TEMPLATE/form.yml","type":"file"},
                       {"name":"sub","path":".github/ISSUE_TEMPLATE/sub","type":"dir"}]"#;
        assert_eq!(
            parse_template_list(list),
            [(
                "bug report".to_string(),
                ".github/ISSUE_TEMPLATE/bug_report.md".to_string()
            )],
            "only Markdown templates can prefill a body"
        );
        assert!(parse_template_list("not json").is_empty());
    }

    #[test]
    fn github_viewed_state_pages_and_the_mutation_uses_variables() {
        let (id, viewed, next) =
            parse_viewed_page(include_str!("../tests/viewed_p1.json")).unwrap();
        assert_eq!(
            (id.as_str(), viewed.as_slice(), next.as_deref()),
            (
                "PR_kwDOexample1",
                ["src/a.rs".to_string()].as_slice(),
                Some("F1")
            )
        );
        let (_, viewed, next) = parse_viewed_page(include_str!("../tests/viewed_p2.json")).unwrap();
        assert_eq!((viewed, next), (vec!["src/d.rs".to_string()], None));
        assert!(parse_viewed_page(r#"{"errors":[{"message":"nope"}]}"#).is_err());
        // GraphQL variables only: the path (even a nasty one) never touches the query text
        let nasty = "dir/\"quote\" $x.rs";
        let m = viewed_mutation("PR_1", nasty, true);
        assert_eq!(&m[..4], ["gh", "api", "graphql", "-f"]);
        assert!(
            m[4].starts_with("query=mutation")
                && m[4].contains("markFileAsViewed")
                && !m[4].contains("quote"),
            "{}",
            m[4]
        );
        assert_eq!(&m[5..], ["-f", "id=PR_1", "-f", &format!("path={nasty}")]);
        assert!(viewed_mutation("PR_1", "a", false)[4].contains("unmarkFileAsViewed"));
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
        let first = comments_query(true, None);
        assert!(
            first.contains("reviewThreads(first:50)")
                && first.contains("comments(first:20)")
                && !first.contains("after:$a"),
            "the first request is the small one: {first}"
        );
        assert!(
            comments_query(true, Some("reviews")).contains("reviews(first:100,after:$a)"),
            "later pages keep 100"
        );
        for q in [&first, &q] {
            assert!(
                q.contains("rateLimit{cost remaining resetAt limit}")
                    && q.contains("reactionGroups{content users{totalCount}}")
                    && !q.contains("nodes{login}"),
                "own queries ask for the quota, and for reaction counts only: {q}"
            );
        }
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
        assert_eq!(q(&["gh", "-b", "a;b `c` *\n#"]), "gh -b 'a;b `c` *\n#'");
    }

    /// Live: a >300-file PR (gh pr diff refuses) must still produce a diff via the files API.
    /// GH_PULSE_LIVE=1 GH_PULSE_HUGE_PR=owner/repo#N cargo test -- --ignored --nocapture
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

    /// Live smoke test: GH_PULSE_LIVE=1 GH_PULSE_REPO=o/r cargo test -- --ignored --nocapture
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
            let items = list(&repo, panel, tab, true)
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
