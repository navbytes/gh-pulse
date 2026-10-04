//! `[[actions]]`: commands you define in `config.toml`, run on the selected row from the `x` menu or a key.
//!
//! The command is yours, so it is trusted like `$EDITOR`. The data it is run on is not: titles, branch
//! names and authors are written by other people. So placeholders only ever become whole `argv` elements
//! (never text pasted into a shell line), and the `shell` form gets everything as `$GHTUI_*` variables
//! instead. Neither form needs quoting, and a branch called `x; rm -rf ~` is just an odd name.
use crate::gh::{Item, Kind};
use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};

pub const MAX_ACTIONS: usize = 30;
pub const MAX_NAME: usize = 40;
const MAX_ARG: usize = 4096;
const MAX_ARGS: usize = 64;
/// How much of a failed command's stderr is kept for the status line.
const STDERR_KEEP: usize = 2048;

/// Where the command runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// gh-tui steps aside, the command gets the terminal (an editor, `claude`, `lazygit`), and gh-tui
    /// comes back when it exits.
    #[default]
    Foreground,
    /// Started and left alone; gh-tui stays usable. For `tmux split-window …`, `roost spawn …`, a browser.
    Detach,
}

/// One `[[actions]]` entry.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CustomCfg {
    /// Shown in the `x` menu.
    pub name: String,
    /// What rows it applies to: `pr`, `issue`, `repo`, `run`, `workflow`, `branch`, `release`, `tag`
    /// or `any`; one word or a list.
    pub on: Strs,
    /// The command as an argument list; `{url}` and friends are filled in per element.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<Vec<String>>,
    /// The command as a `sh -c` script, for pipes and `&&`. Row data arrives as `$GHTUI_*` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default)]
    pub mode: Mode,
    /// A key that runs it from the list (`"ctrl-r"`, `"R"`), checked against every built-in key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Foreground only: wait for Enter before coming back. Unset: only when the command failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause: Option<bool>,
    /// Show the full command and ask first.
    #[serde(default)]
    pub confirm: bool,
}

/// `on = "pr"` or `on = ["pr", "issue"]`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Strs {
    One(String),
    Many(Vec<String>),
}

impl Strs {
    fn list(&self) -> Vec<&str> {
        match self {
            Strs::One(s) => vec![s.as_str()],
            Strs::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

const KINDS: &str = "pr, issue, repo, run, workflow, branch, release, tag, any";

/// What `on` names a row as; `None` for rows no action applies to (files, checks, comments...).
fn kind_word(k: Kind) -> Option<&'static str> {
    Some(match k {
        Kind::Pr => "pr",
        Kind::Issue => "issue",
        Kind::Run => "run",
        Kind::Workflow => "workflow",
        Kind::Branch => "branch",
        Kind::Release => "release",
        Kind::Tag => "tag",
        Kind::Repo => "repo",
        _ => return None,
    })
}

impl CustomCfg {
    /// Does this action apply to a row of kind `k`?
    pub fn applies_to(&self, k: Kind) -> bool {
        let Some(w) = kind_word(k) else { return false };
        self.on
            .list()
            .iter()
            .any(|o| o.eq_ignore_ascii_case("any") || o.eq_ignore_ascii_case(w))
    }

    /// Everything a bad entry can get wrong, as a message naming the offending key.
    pub fn check(&self) -> Result<(), (&'static str, String)> {
        let n = self.name.chars().count();
        if self.name.trim().is_empty() || n > MAX_NAME || self.name.chars().any(char::is_control) {
            return Err((
                "name",
                format!("actions.name: must be 1..{MAX_NAME} characters, not blank, got {n}"),
            ));
        }
        let on = self.on.list();
        if on.is_empty() {
            return Err(("on", format!("actions.on: name at least one of {KINDS}")));
        }
        for o in on {
            if !(o.eq_ignore_ascii_case("any")
                || [
                    "pr", "issue", "repo", "run", "workflow", "branch", "release", "tag",
                ]
                .iter()
                .any(|k| o.eq_ignore_ascii_case(k)))
            {
                return Err(("on", format!("actions.on: {o:?} is not one of {KINDS}")));
            }
        }
        match (&self.run, &self.shell) {
            (Some(_), Some(_)) => {
                return Err(("shell", "actions: give run or shell, not both".to_string()));
            }
            (None, None) => {
                return Err((
                    "name",
                    format!(
                        "actions {:?}: give run = [\"cmd\", ...] or shell = \"...\"",
                        self.name
                    ),
                ));
            }
            (Some(run), None) => {
                if run.is_empty() || run[0].trim().is_empty() {
                    return Err((
                        "run",
                        "actions.run: the first element is the program to run".into(),
                    ));
                }
                if run.len() > MAX_ARGS || run.iter().any(|a| a.chars().count() > MAX_ARG) {
                    return Err((
                        "run",
                        format!("actions.run: at most {MAX_ARGS} elements of {MAX_ARG} characters"),
                    ));
                }
                if run.iter().any(|a| a.contains('\0')) {
                    return Err(("run", "actions.run: no NUL characters".into()));
                }
                for a in run {
                    expand(a, &mut |n| Vars::default().get(n).map(str::to_string))
                        .map_err(|e| ("run", format!("actions.run: {e}")))?;
                }
            }
            (None, Some(s)) => {
                if s.trim().is_empty() || s.chars().count() > MAX_ARG || s.contains('\0') {
                    return Err((
                        "shell",
                        format!("actions.shell: must be 1..{MAX_ARG} characters, no NUL"),
                    ));
                }
            }
        }
        if self.mode == Mode::Detach && self.pause.is_some() {
            return Err((
                "pause",
                "actions.pause: only applies to mode = \"foreground\"".into(),
            ));
        }
        Ok(())
    }
}

/// The values a command can be given for one row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Vars {
    pub url: String,
    pub repo: String,
    pub owner: String,
    pub name: String,
    pub number: String,
    pub kind: String,
    pub author: String,
    /// The exact branch / tag / release name (blank for other rows).
    pub refname: String,
    pub title: String,
    pub state: String,
}

impl Vars {
    pub fn of(it: &Item) -> Vars {
        let (owner, name) = it.repo.split_once('/').unwrap_or(("", &it.repo));
        Vars {
            url: it.url.clone(),
            repo: it.repo.clone(),
            owner: owner.into(),
            name: name.into(),
            number: if it.number == 0 {
                String::new()
            } else {
                it.number.to_string()
            },
            kind: kind_word(it.kind).unwrap_or_default().into(),
            author: it.author.login.clone(),
            refname: match it.kind {
                Kind::Branch | Kind::Tag | Kind::Release => it.cmd_name().into(),
                _ => String::new(),
            },
            title: it.title.clone(),
            state: it.state.to_lowercase(),
        }
    }

    fn get(&self, name: &str) -> Option<&str> {
        Some(match name {
            "url" => &self.url,
            "repo" => &self.repo,
            "owner" => &self.owner,
            "name" => &self.name,
            "number" => &self.number,
            "kind" => &self.kind,
            "author" => &self.author,
            "ref" => &self.refname,
            _ => return None,
        })
    }

    /// The `$GHTUI_*` variables every command gets, title included (an env var is data, never code).
    fn env(&self) -> Vec<(String, String)> {
        let clean = |s: &str| {
            s.replace('\0', "")
                .chars()
                .take(MAX_ARG)
                .collect::<String>()
        };
        [
            ("GHTUI_URL", &self.url),
            ("GHTUI_REPO", &self.repo),
            ("GHTUI_OWNER", &self.owner),
            ("GHTUI_NAME", &self.name),
            ("GHTUI_NUMBER", &self.number),
            ("GHTUI_KIND", &self.kind),
            ("GHTUI_AUTHOR", &self.author),
            ("GHTUI_REF", &self.refname),
            ("GHTUI_TITLE", &self.title),
            ("GHTUI_STATE", &self.state),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), clean(v)))
        .collect()
    }
}

/// `{url}` -> value, `{{` / `}}` -> a literal brace. Anything else in braces is an error, so a typo
/// shows up when the config loads and never as a half-filled command.
fn expand(t: &str, get: &mut dyn FnMut(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::with_capacity(t.len());
    let mut cs = t.chars().peekable();
    while let Some(c) = cs.next() {
        match c {
            '{' if cs.peek() == Some(&'{') => {
                cs.next();
                out.push('{');
            }
            '}' if cs.peek() == Some(&'}') => {
                cs.next();
                out.push('}');
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match cs.next() {
                        Some('}') => break,
                        Some(c) if c.is_ascii_alphanumeric() || c == '_' => name.push(c),
                        _ => {
                            return Err(format!(
                                "unbalanced brace in {t:?}: write {{{{ or }}}} for a literal one"
                            ));
                        }
                    }
                }
                match get(&name) {
                    Some(v) => out.push_str(&v),
                    None => {
                        return Err(format!(
                            "unknown placeholder {{{name}}} (use url, repo, owner, name, number, kind, author, ref)"
                        ));
                    }
                }
            }
            '}' => {
                return Err(format!(
                    "unbalanced brace in {t:?}: write {{{{ or }}}} for a literal one"
                ));
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

/// A command ready to run on one row.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub name: String,
    /// Program and arguments exactly as they will be executed.
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    /// What the confirm popup shows (the script itself for `shell`).
    pub shown: String,
    pub mode: Mode,
    pub pause: Option<bool>,
}

impl CustomCfg {
    /// Fill the command in for a row. Refuses when a placeholder it uses has no value on this row
    /// (`{number}` on a branch), rather than running with a hole in it.
    pub fn prepare(&self, v: &Vars) -> Result<Prepared, String> {
        let env = v.env();
        let (argv, shown) = match (&self.run, &self.shell) {
            (Some(run), _) => {
                let mut empty: Vec<String> = vec![];
                let argv: Vec<String> = run
                    .iter()
                    .map(|a| {
                        expand(a, &mut |n| {
                            let val = v.get(n)?;
                            if val.is_empty() && !empty.iter().any(|e| e == n) {
                                empty.push(n.to_string());
                            }
                            Some(val.to_string())
                        })
                    })
                    .collect::<Result<_, _>>()?;
                if let Some(n) = empty.first() {
                    return Err(format!(
                        "{}: this row has no {{{n}}} ({} rows don't)",
                        self.name,
                        if v.kind.is_empty() { "these" } else { &v.kind }
                    ));
                }
                let shown = crate::gh::shell(&argv);
                (argv, shown)
            }
            (None, Some(s)) => (
                vec!["sh".into(), "-c".into(), s.clone(), "gh-tui".into()],
                s.clone(),
            ),
            (None, None) => return Err(format!("{}: no command", self.name)),
        };
        Ok(Prepared {
            name: self.name.clone(),
            argv,
            env,
            shown,
            mode: self.mode,
            pause: self.pause,
        })
    }
}

impl Prepared {
    fn command(&self) -> Command {
        let mut c = Command::new(&self.argv[0]);
        c.args(&self.argv[1..]).envs(self.env.iter().cloned());
        c
    }
}

/// Run in the foreground with the real terminal (the caller has already handed it over). Returns the
/// status line to show afterwards.
pub fn run_foreground(p: &Prepared) -> String {
    #[cfg(unix)]
    ignore_interrupts();
    let result = p.command().status();
    let (line, failed) = match &result {
        Ok(s) if s.success() => (format!("{}: done", p.name), false),
        Ok(s) => (format!("{}: {}", p.name, exit_text(s)), true),
        Err(e) => (format!("{}: can't run {}: {e}", p.name, p.argv[0]), true),
    };
    if p.pause.unwrap_or(failed) {
        use std::io::{BufRead, Write};
        let mut out = std::io::stdout();
        let _ = write!(out, "\n[gh-tui] {line} - press Enter to return ");
        let _ = out.flush();
        let _ = std::io::stdin().lock().read_line(&mut String::new());
    }
    line
}

fn exit_text(s: &std::process::ExitStatus) -> String {
    match s.code() {
        Some(c) => format!("exited with status {c}"),
        None => "was stopped by a signal".into(),
    }
}

/// While a foreground command owns the terminal, Ctrl-C reaches gh-tui too (same process group, and the
/// terminal is in line mode). The program under it must still get it, so the signal is only noted here:
/// a handler is reset to the default on `exec`, an ignored signal would not be.
#[cfg(unix)]
fn ignore_interrupts() {
    use std::sync::{Arc, OnceLock, atomic::AtomicBool};
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    FLAG.get_or_init(|| {
        let f = Arc::new(AtomicBool::new(false));
        let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, f.clone());
        f
    });
}

/// Start without waiting: no stdin, no stdout, stderr kept for a failure message. A thread collects the
/// exit (no zombies) and reports a failure; a command that succeeds says nothing more.
pub fn spawn_detached(p: &Prepared, done: impl FnOnce(Result<(), String>) + Send + 'static) {
    let mut c = p.command();
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // its own process group: a Ctrl-C meant for the terminal never reaches it, and ours never kills it
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut c, 0);
    let (name, prog) = (p.name.clone(), p.argv[0].clone());
    match c.spawn() {
        Err(e) => done(Err(format!("{name}: can't run {prog}: {e}"))),
        Ok(mut child) => {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    let mut buf = vec![];
                    let _ = s.by_ref().take(STDERR_KEEP as u64).read_to_end(&mut buf);
                    // keep draining so a chatty command never blocks on a full pipe
                    let _ = std::io::copy(&mut s, &mut std::io::sink());
                    err = String::from_utf8_lossy(&buf).into_owned();
                }
                match child.wait() {
                    Ok(s) if s.success() => done(Ok(())),
                    Ok(s) => {
                        let first = err.lines().rev().find(|l| !l.trim().is_empty());
                        done(Err(match first {
                            Some(l) => format!("{name}: {}: {}", exit_text(&s), l.trim()),
                            None => format!("{name}: {}", exit_text(&s)),
                        }));
                    }
                    Err(e) => done(Err(format!("{name}: {e}"))),
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(toml: &str) -> CustomCfg {
        toml::from_str(toml).unwrap()
    }

    fn vars() -> Vars {
        Vars {
            url: "https://github.com/o/r/pull/7".into(),
            repo: "o/r".into(),
            owner: "o".into(),
            name: "r".into(),
            number: "7".into(),
            kind: "pr".into(),
            author: "octocat".into(),
            refname: String::new(),
            title: "x; rm -rf ~ $(touch /tmp/pwn)".into(),
            state: "open".into(),
        }
    }

    #[test]
    fn placeholders_fill_whole_arguments_and_braces_can_be_escaped() {
        let a = cfg("name='n'\non='pr'\nrun=['claude','Review {url} ({repo}#{number})','{{x}}']");
        let p = a.prepare(&vars()).unwrap();
        assert_eq!(
            p.argv,
            [
                "claude",
                "Review https://github.com/o/r/pull/7 (o/r#7)",
                "{x}"
            ]
        );
    }

    #[test]
    fn row_data_never_becomes_shell_text() {
        // a hostile title or author is one argv element, or an env var, never parsed by a shell
        let mut v = vars();
        v.author = "a'; reboot #".into();
        let a = cfg("name='n'\non='pr'\nrun=['echo','{author}']");
        assert_eq!(a.prepare(&v).unwrap().argv, ["echo", "a'; reboot #"]);
        let s = cfg("name='n'\non='pr'\nshell='echo \"$GHTUI_TITLE\"'");
        let p = s.prepare(&v).unwrap();
        assert_eq!(p.argv[..2], ["sh", "-c"]);
        assert_eq!(
            p.argv[2], "echo \"$GHTUI_TITLE\"",
            "script is exactly as written"
        );
        assert!(p.env.contains(&("GHTUI_TITLE".into(), v.title.clone())));
        assert!(p.env.contains(&("GHTUI_NUMBER".into(), "7".into())));
    }

    #[test]
    fn a_typo_or_stray_brace_is_a_config_error() {
        for bad in ["{urll}", "{url", "url}", "{}", "{a b}"] {
            let a = cfg(&format!("name='n'\non='pr'\nrun=['x','{bad}']"));
            assert!(a.check().is_err(), "{bad}");
        }
        assert!(cfg("name='n'\non='pr'\nrun=['x','{url}']").check().is_ok());
    }

    #[test]
    fn a_placeholder_the_row_lacks_refuses_the_run() {
        let mut v = vars();
        v.number = String::new();
        v.kind = "branch".into();
        let a = cfg("name='n'\non='any'\nrun=['x','{number}']");
        let e = a.prepare(&v).unwrap_err();
        assert!(e.contains("{number}") && e.contains("branch"), "{e}");
        // unused empties are fine
        assert!(
            cfg("name='n'\non='any'\nrun=['x','{repo}']")
                .prepare(&v)
                .is_ok()
        );
    }

    #[test]
    fn entries_are_validated() {
        let bad = |t: &str| cfg(t).check().unwrap_err().1;
        assert!(bad("name=''\non='pr'\nrun=['x']").contains("name"));
        assert!(bad("name='n'\non='prs'\nrun=['x']").contains("prs"));
        assert!(bad("name='n'\non=[]\nrun=['x']").contains("on"));
        assert!(bad("name='n'\non='pr'").contains("run"));
        assert!(bad("name='n'\non='pr'\nrun=['x']\nshell='y'").contains("not both"));
        assert!(bad("name='n'\non='pr'\nrun=[]").contains("program"));
        assert!(bad("name='n'\non='pr'\nrun=['x']\nmode='detach'\npause=true").contains("pause"));
        assert!(
            toml::from_str::<CustomCfg>("name='n'\non='pr'\nrun=['x']\nbogus=1").is_err(),
            "unknown keys are errors"
        );
        assert!(
            cfg("name='n'\non=['pr','Issue']\nrun=['x']")
                .check()
                .is_ok()
        );
    }

    #[test]
    fn on_matches_rows_by_kind() {
        let a = cfg("name='n'\non=['pr','issue']\nrun=['x']");
        assert!(a.applies_to(Kind::Pr) && a.applies_to(Kind::Issue));
        assert!(!a.applies_to(Kind::Branch) && !a.applies_to(Kind::File));
        let any = cfg("name='n'\non='any'\nrun=['x']");
        assert!(any.applies_to(Kind::Release) && any.applies_to(Kind::Repo));
        assert!(
            !any.applies_to(Kind::File),
            "derived rows never offer actions"
        );
    }

    #[test]
    fn foreground_runs_report_their_exit_and_detached_ones_report_failures_only() {
        let ok = cfg("name='ok'\non='pr'\nrun=['sh','-c','exit 0']\npause=false")
            .prepare(&vars())
            .unwrap();
        assert_eq!(run_foreground(&ok), "ok: done");
        let bad = cfg("name='bad'\non='pr'\nrun=['sh','-c','exit 3']\npause=false")
            .prepare(&vars())
            .unwrap();
        assert_eq!(run_foreground(&bad), "bad: exited with status 3");
        let gone = cfg("name='g'\non='pr'\nrun=['/nonexistent/prog']\npause=false")
            .prepare(&vars())
            .unwrap();
        assert!(run_foreground(&gone).contains("can't run"));

        let (tx, rx) = std::sync::mpsc::channel();
        let t = tx.clone();
        let d = cfg("name='d'\non='pr'\nmode='detach'\nshell='echo oops >&2; exit 2'")
            .prepare(&vars())
            .unwrap();
        spawn_detached(&d, move |r| t.send(r).unwrap());
        let e = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
            .unwrap_err();
        assert_eq!(e, "d: exited with status 2: oops");
        let d = cfg("name='d'\non='pr'\nmode='detach'\nrun=['true']")
            .prepare(&vars())
            .unwrap();
        spawn_detached(&d, move |r| tx.send(r).unwrap());
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap(),
            Ok(())
        );
    }

    #[test]
    fn env_reaches_the_command() {
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = std::env::temp_dir().join(format!("gh-tui-custom-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out");
        let a = cfg(&format!(
            "name='e'\non='pr'\nmode='detach'\nshell='printf %s \"$GHTUI_REPO#$GHTUI_NUMBER:$GHTUI_AUTHOR\" > {}'",
            out.display()
        ))
        .prepare(&vars())
        .unwrap();
        spawn_detached(&a, move |r| tx.send(r).unwrap());
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "o/r#7:octocat");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
