use crate::gh::{Item, Kind, run_ids};

pub type Build = Box<dyn Fn(&str) -> Vec<String>>;

/// A mutation. `build` turns the (possibly empty) prompt text into the full command line.
pub struct Action {
    pub label: String,
    /// (popup title, input required)
    pub prompt: Option<(&'static str, bool)>,
    pub build: Build,
    /// Repo the cwd must be a clone of (checked when the command runs).
    pub local_of: Option<String>,
    /// Opens a form instead of running `build`.
    pub form: Option<FormKind>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FormKind {
    Issue,
    /// New PR from this (raw) branch name.
    Pr(String),
    /// New PR from the branch checked out in the current directory.
    PrCwd,
    /// Run this workflow: (numeric id, display name, file path).
    Dispatch(String, String, String),
}

fn form_act(label: &str, kind: FormKind) -> Action {
    Action {
        label: label.into(),
        prompt: None,
        build: Box::new(|_| vec![]),
        local_of: None,
        form: Some(kind),
    }
}

/// What the detail pane has selected, for tab-specific actions.
#[derive(Default)]
pub struct Sel {
    pub check_link: Option<String>,
    /// (thread id, resolved)
    pub thread: Option<(String, bool)>,
    /// (head sha, path, line, side)
    pub inline: Option<(String, String, u32, &'static str)>,
}

macro_rules! cmd {
    ($($e:expr),* $(,)?) => { vec![$(String::from($e)),*] };
}

fn act(
    label: impl Into<String>,
    prompt: Option<(&'static str, bool)>,
    build: impl Fn(&str) -> Vec<String> + 'static,
) -> Action {
    Action {
        label: label.into(),
        prompt,
        build: Box::new(build),
        local_of: None,
        form: None,
    }
}

/// Percent-encodes each `/`-separated segment so `feat#2` or `a?b` can't alter the API path.
pub fn enc_path(s: &str) -> String {
    let seg = |p: &str| {
        p.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect::<String>()
    };
    s.split('/').map(seg).collect::<Vec<_>>().join("/")
}

/// Marks one notification thread read (the inbox's `m`).
pub fn mark_read(thread_id: &str) -> Vec<String> {
    cmd![
        "gh",
        "api",
        "-X",
        "PATCH",
        format!("notifications/threads/{}", enc_path(thread_id))
    ]
}

fn body_flag(b: &str) -> Vec<String> {
    if b.is_empty() { vec![] } else { cmd!["-b", b] }
}

const REPLY: &str = "mutation($t:ID!,$b:String!){addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$t,body:$b}){comment{id}}}";
const RESOLVE: &str = "mutation($t:ID!){resolveReviewThread(input:{threadId:$t}){thread{id}}}";
const UNRESOLVE: &str = "mutation($t:ID!){unresolveReviewThread(input:{threadId:$t}){thread{id}}}";

pub fn actions(it: Option<&Item>, panel: usize, repo: &str, sel: &Sel) -> Vec<Action> {
    let mut v = vec![];
    match panel {
        1 => v.push(form_act(
            "New pull request from the current branch...",
            FormKind::PrCwd,
        )),
        2 => v.push(form_act("New issue...", FormKind::Issue)),
        5 => {
            let r = repo.to_string();
            v.push(act(
                "New release (auto notes)",
                Some(("New release tag", true)),
                move |t| {
                    cmd![
                        "gh",
                        "release",
                        "create",
                        "-R",
                        &r,
                        "--generate-notes",
                        "--",
                        t
                    ]
                },
            ));
        }
        _ => {}
    }
    let Some(it) = it else { return v };
    // a repo name that isn't plain owner/name never reaches a command line
    if !it.repo_ok() {
        return vec![];
    }
    let (r, n) = (it.repo.clone(), it.number.to_string());
    let open = it.state.eq_ignore_ascii_case("open");
    let edit = |view: &'static str, label: &'static str, flag: &'static str, r: &str, n: &str| {
        let (r, n) = (r.to_string(), n.to_string());
        act(label, Some((label, true)), move |x| {
            cmd!["gh", view, "edit", &n, "-R", &r, flag, x]
        })
    };
    match it.kind {
        Kind::Pr => {
            let (r1, n1) = (r.clone(), n.clone());
            v.push(act(
                "Approve",
                Some(("Approve (optional comment)", false)),
                move |b| {
                    [
                        cmd!["gh", "pr", "review", &n1, "-R", &r1, "--approve"],
                        body_flag(b),
                    ]
                    .concat()
                },
            ));
            let (r1, n1) = (r.clone(), n.clone());
            v.push(act(
                "Request changes",
                Some(("Request changes: explain", true)),
                move |b| {
                    cmd![
                        "gh",
                        "pr",
                        "review",
                        &n1,
                        "-R",
                        &r1,
                        "--request-changes",
                        "-b",
                        b
                    ]
                },
            ));
            let (r1, n1) = (r.clone(), n.clone());
            v.push(act("Comment on PR", Some(("Comment", true)), move |b| {
                cmd!["gh", "pr", "comment", &n1, "-R", &r1, "-b", b]
            }));
            if open {
                for m in ["squash", "merge", "rebase"] {
                    let (r1, n1) = (r.clone(), n.clone());
                    v.push(act(format!("Merge ({m})"), None, move |_| {
                        cmd!["gh", "pr", "merge", &n1, "-R", &r1, format!("--{m}")]
                    }));
                }
                let (r1, n1) = (r.clone(), n.clone());
                v.push(act("Close PR", None, move |_| {
                    cmd!["gh", "pr", "close", &n1, "-R", &r1]
                }));
                let (r1, n1, draft) = (r.clone(), n.clone(), it.is_draft);
                let label = if draft {
                    "Mark ready for review"
                } else {
                    "Convert to draft"
                };
                v.push(act(label, None, move |_| {
                    let mut c = cmd!["gh", "pr", "ready", &n1, "-R", &r1];
                    if !draft {
                        c.push("--undo".into());
                    }
                    c
                }));
            } else if it.state.eq_ignore_ascii_case("closed") {
                let (r1, n1) = (r.clone(), n.clone());
                v.push(act("Reopen PR", None, move |_| {
                    cmd!["gh", "pr", "reopen", &n1, "-R", &r1]
                }));
            }
            v.push(edit("pr", "Add label", "--add-label", &r, &n));
            v.push(edit("pr", "Remove label", "--remove-label", &r, &n));
            v.push(edit("pr", "Add assignee", "--add-assignee", &r, &n));
            v.push(edit("pr", "Request reviewer", "--add-reviewer", &r, &n));
            if let Some((run, _)) = sel.check_link.as_deref().and_then(run_ids) {
                for (label, failed) in [
                    ("Re-run failed jobs of this check's run", true),
                    ("Re-run all jobs of this check's run", false),
                ] {
                    let (r1, run) = (r.clone(), run.to_string());
                    v.push(act(label, None, move |_| {
                        let mut c = cmd!["gh", "run", "rerun", &run, "-R", &r1];
                        if failed {
                            c.push("--failed".into());
                        }
                        c
                    }));
                }
            }
            if let Some((id, resolved)) = sel.thread.clone() {
                let id1 = id.clone();
                v.push(act("Reply to thread", Some(("Reply", true)), move |b| {
                    cmd![
                        "gh",
                        "api",
                        "graphql",
                        "-f",
                        format!("query={REPLY}"),
                        "-f",
                        format!("t={id1}"),
                        "-f",
                        format!("b={b}")
                    ]
                }));
                let (label, q) = if resolved {
                    ("Unresolve thread", UNRESOLVE)
                } else {
                    ("Resolve thread", RESOLVE)
                };
                v.push(act(label, None, move |_| {
                    cmd![
                        "gh",
                        "api",
                        "graphql",
                        "-f",
                        format!("query={q}"),
                        "-f",
                        format!("t={id}")
                    ]
                }));
            }
            if let Some((sha, path, line, side)) = sel.inline.clone() {
                let (r1, n1) = (r.clone(), n.clone());
                v.push(act(
                    format!("Comment on {}:{line}", crate::sanitize::clean(&path)),
                    Some(("Inline comment", true)),
                    move |b| {
                        cmd![
                            "gh",
                            "api",
                            format!("repos/{r1}/pulls/{n1}/comments"),
                            "-f",
                            format!("commit_id={sha}"),
                            "-f",
                            format!("path={path}"),
                            "-F",
                            format!("line={line}"),
                            "-f",
                            format!("side={side}"),
                            "-f",
                            format!("body={b}")
                        ]
                    },
                ));
            }
        }
        Kind::Issue => {
            let (r1, n1) = (r.clone(), n.clone());
            v.push(act("Comment on issue", Some(("Comment", true)), move |b| {
                cmd!["gh", "issue", "comment", &n1, "-R", &r1, "-b", b]
            }));
            let (r1, n1) = (r.clone(), n.clone());
            if open {
                v.push(act("Close issue", None, move |_| {
                    cmd!["gh", "issue", "close", &n1, "-R", &r1]
                }));
            } else {
                v.push(act("Reopen issue", None, move |_| {
                    cmd!["gh", "issue", "reopen", &n1, "-R", &r1]
                }));
            }
            v.push(edit("issue", "Add label", "--add-label", &r, &n));
            v.push(edit("issue", "Remove label", "--remove-label", &r, &n));
            v.push(edit("issue", "Add assignee", "--add-assignee", &r, &n));
        }
        Kind::Workflow if it.state == "active" => v.push(form_act(
            "Run workflow...",
            FormKind::Dispatch(it.number.to_string(), it.title.clone(), it.cmd.clone()),
        )),
        Kind::Run => {
            let live = matches!(
                it.state.as_str(),
                "in_progress" | "queued" | "pending" | "waiting" | "requested"
            );
            if live {
                let (r1, n1) = (r.clone(), n.clone());
                v.push(act("Cancel run", None, move |_| {
                    cmd!["gh", "run", "cancel", &n1, "-R", &r1]
                }));
            } else {
                for (label, flag) in [
                    ("Re-run all jobs", None),
                    ("Re-run failed jobs", Some("--failed")),
                ] {
                    let (r1, n1) = (r.clone(), n.clone());
                    v.push(act(label, None, move |_| {
                        let mut c = cmd!["gh", "run", "rerun", &n1, "-R", &r1];
                        c.extend(flag.map(String::from));
                        c
                    }));
                }
            }
        }
        Kind::Branch if it.cmd_name().starts_with('-') => {}
        Kind::Branch => {
            let name = it.cmd_name().to_string();
            let name1 = name.clone();
            let mut sw = act("Switch to branch (git, in cwd)", None, move |_| {
                cmd!["git", "switch", &name1]
            });
            sw.local_of = Some(r.clone());
            v.push(sw);
            if it.number == 0 {
                v.push(form_act(
                    "Create pull request...",
                    FormKind::Pr(name.clone()),
                ));
            }
            v.push(act("Delete remote branch", None, move |_| {
                cmd![
                    "gh",
                    "api",
                    "-X",
                    "DELETE",
                    format!("repos/{r}/git/refs/heads/{}", enc_path(&name))
                ]
            }));
        }
        Kind::Release => {
            let (r1, tag) = (r.clone(), it.cmd_name().to_string());
            v.push(act("Download assets to cwd", None, move |_| {
                cmd![
                    "gh", "release", "download", "-R", &r1, "-D", ".", "--", &tag
                ]
            }));
            let tag = it.cmd_name().to_string();
            v.push(act("Delete release", None, move |_| {
                cmd!["gh", "release", "delete", "-R", &r, "-y", "--", &tag]
            }));
        }
        _ => {}
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(state: &str) -> Item {
        Item {
            number: 7,
            state: state.into(),
            repo: "o/r".into(),
            kind: Kind::Pr,
            ..Default::default()
        }
    }

    fn find(v: &[Action], label: &str) -> Vec<String> {
        let a = v
            .iter()
            .find(|a| a.label.starts_with(label))
            .unwrap_or_else(|| panic!("no {label}"));
        (a.build)("hello")
    }

    /// Displayed names have look-alike characters neutralized; commands must still carry the real
    /// name, and the confirm popup (gh::shell) shows it neutralized.
    #[test]
    fn commands_use_raw_names_and_the_popup_shows_them_neutralized() {
        let tag = "v1\u{200b}.0";
        let rel = Item {
            title: "v1<U+200B>.0".into(),
            meta: "v1<U+200B>.0".into(),
            cmd: tag.into(),
            repo: "o/r".into(),
            kind: Kind::Release,
            ..Default::default()
        };
        let v = actions(Some(&rel), 5, "o/r", &Sel::default());
        let del = find(&v, "Delete release");
        assert_eq!(
            del.last().map(String::as_str),
            Some(tag),
            "the real tag reaches gh"
        );
        assert_eq!(
            crate::gh::shell(&del),
            "gh release delete -R o/r -y -- 'v1<U+200B>.0'"
        );
        let br = Item {
            title: "feat/a<U+202E>b".into(),
            cmd: "feat/a\u{202e}b".into(),
            repo: "o/r".into(),
            kind: Kind::Branch,
            ..Default::default()
        };
        let v = actions(Some(&br), 4, "o/r", &Sel::default());
        assert_eq!(find(&v, "Switch")[2], "feat/a\u{202e}b");
        assert!(
            find(&v, "Delete remote")[4].ends_with("feat/a%E2%80%AEb"),
            "path is percent-encoded from the raw name"
        );
        // inline comments carry the real file path (Sel is filled from diff::File::raw_path)
        let sel = Sel {
            inline: Some(("abc".into(), "dir/we\u{200b}ird.rs".into(), 3, "RIGHT")),
            ..Default::default()
        };
        let pr = Item {
            number: 7,
            state: "OPEN".into(),
            repo: "o/r".into(),
            kind: Kind::Pr,
            ..Default::default()
        };
        let v = actions(Some(&pr), 1, "o/r", &sel);
        assert!(
            find(&v, "Comment on dir/we<U+200B>ird.rs:3")
                .contains(&"path=dir/we\u{200b}ird.rs".to_string())
        );
    }

    #[test]
    fn encodes_branch_paths() {
        assert_eq!(enc_path("feat#2"), "feat%232");
        assert_eq!(enc_path("a/b c?d%e"), "a/b%20c%3Fd%25e");
        assert_eq!(enc_path("ünï"), "%C3%BCn%C3%AF");
        let b = Item {
            title: "feat#2".into(),
            repo: "o/r".into(),
            kind: Kind::Branch,
            ..Default::default()
        };
        let v = actions(Some(&b), 4, "o/r", &Sel::default());
        assert_eq!(
            find(&v, "Delete remote")[4],
            "repos/o/r/git/refs/heads/feat%232"
        );
        assert_eq!(find(&v, "Switch")[..2], ["git", "switch"]);
        assert_eq!(
            v.iter()
                .find(|a| a.label.starts_with("Switch"))
                .unwrap()
                .local_of
                .as_deref(),
            Some("o/r")
        );
        let dash = Item {
            title: "-x".into(),
            kind: Kind::Branch,
            ..Default::default()
        };
        assert!(actions(Some(&dash), 4, "o/r", &Sel::default()).is_empty());
        let rel = Item {
            meta: "-v1".into(),
            repo: "o/r".into(),
            kind: Kind::Release,
            ..Default::default()
        };
        let v = actions(Some(&rel), 5, "o/r", &Sel::default());
        assert_eq!(
            find(&v, "Delete release").join(" "),
            "gh release delete -R o/r -y -- -v1"
        );
    }

    #[test]
    fn run_actions_follow_run_state() {
        let run = |state: &str| Item {
            number: 5,
            state: state.into(),
            repo: "o/r".into(),
            kind: Kind::Run,
            ..Default::default()
        };
        let labels = |s: &str| {
            actions(Some(&run(s)), 3, "o/r", &Sel::default())
                .into_iter()
                .map(|a| a.label)
                .collect::<Vec<_>>()
        };
        assert_eq!(labels("in_progress"), ["Cancel run"]);
        assert_eq!(labels("queued"), ["Cancel run"]);
        assert_eq!(labels("failure"), ["Re-run all jobs", "Re-run failed jobs"]);
        assert_eq!(labels("success"), ["Re-run all jobs", "Re-run failed jobs"]);
    }

    #[test]
    fn command_shapes() {
        let sel = Sel {
            check_link: Some("https://github.com/o/r/actions/runs/11/job/22".into()),
            thread: Some(("PRRT_1".into(), false)),
            inline: Some(("abc".into(), "a.rs".into(), 5, "RIGHT")),
        };
        let v = actions(Some(&pr("OPEN")), 1, "o/r", &sel);
        assert_eq!(
            find(&v, "Approve").join(" "),
            "gh pr review 7 -R o/r --approve -b hello"
        );
        assert_eq!(
            find(&v, "Merge (rebase)").join(" "),
            "gh pr merge 7 -R o/r --rebase"
        );
        assert_eq!(
            find(&v, "Re-run failed").join(" "),
            "gh run rerun 11 -R o/r --failed"
        );
        assert_eq!(
            find(&v, "Comment on a.rs:5").join(" "),
            "gh api repos/o/r/pulls/7/comments -f commit_id=abc -f path=a.rs -F line=5 -f side=RIGHT -f body=hello"
        );
        assert!(find(&v, "Reply")[4].starts_with("query=mutation"));
        assert!(find(&v, "Resolve thread").contains(&"t=PRRT_1".to_string()));
        assert!(
            actions(Some(&pr("MERGED")), 1, "o/r", &Sel::default())
                .iter()
                .all(|a| !a.label.starts_with("Merge"))
        );
        assert_eq!(
            crate::gh::shell(&find(&v, "Request changes")),
            "gh pr review 7 -R o/r --request-changes -b hello"
        );
    }
}
