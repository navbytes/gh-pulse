//! Where gh-pulse opens: a repo, or the global home. One pure decision, so the table of cases is a test.
use crate::config::StartMode;

#[derive(Debug, PartialEq, Eq)]
pub enum Start {
    /// The repo home for this repo.
    Repo(String),
    /// The global home; `repo` is the repo context `G` can lead to (a clone, or `-R`).
    Global { repo: Option<String> },
}

pub const NOT_A_REPO: &str = "not in a GitHub repo; pass -R owner/repo, or use --start global";

/// `mode` is the flag if given, else `[ui] start`. `cwd_repo` is only asked when the answer matters
/// and can cost an API call, so `-R` never triggers it; `local_repo` is the offline guess (git
/// remotes) used to give a global start its repo context.
pub fn decide(
    mode: StartMode,
    repo_flag: Option<String>,
    cwd_repo: impl FnOnce() -> Option<String>,
    local_repo: impl FnOnce() -> Option<String>,
) -> Result<Start, String> {
    match mode {
        StartMode::Global => Ok(Start::Global {
            repo: repo_flag.or_else(local_repo),
        }),
        StartMode::Repo => repo_flag
            .or_else(cwd_repo)
            .map(Start::Repo)
            .ok_or_else(|| NOT_A_REPO.to_string()),
        StartMode::Auto => Ok(match repo_flag.or_else(cwd_repo) {
            Some(r) => Start::Repo(r),
            None => Start::Global { repo: None },
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// (mode, -R, directory is a clone of, offline guess) -> (start, API lookups made)
    fn run(
        mode: StartMode,
        flag: Option<&str>,
        cwd: Option<&str>,
        local: Option<&str>,
    ) -> (Result<Start, String>, usize, usize) {
        let (a, b) = (Cell::new(0), Cell::new(0));
        let r = decide(
            mode,
            flag.map(String::from),
            || {
                a.set(a.get() + 1);
                cwd.map(String::from)
            },
            || {
                b.set(b.get() + 1);
                local.map(String::from)
            },
        );
        (r, a.get(), b.get())
    }

    #[test]
    fn start_decision_table() {
        use StartMode::*;
        let repo = |r: &str| Ok(Start::Repo(r.into()));
        let global = |r: Option<&str>| Ok(Start::Global { repo: r.map(String::from) });
        // auto: a flag or a clone means the repo; otherwise the global home, never an error
        assert_eq!(run(Auto, Some("o/r"), Some("x/y"), None), (repo("o/r"), 0, 0), "-R wins, no lookup");
        assert_eq!(run(Auto, None, Some("x/y"), None), (repo("x/y"), 1, 0));
        assert_eq!(run(Auto, None, None, Some("never/used")), (global(None), 1, 0));
        // repo: like today; outside a clone it is an error
        assert_eq!(run(Repo, Some("o/r"), None, None), (repo("o/r"), 0, 0));
        assert_eq!(run(Repo, None, Some("x/y"), None), (repo("x/y"), 1, 0));
        let (e, _, _) = run(Repo, None, None, None);
        assert_eq!(e, Err(NOT_A_REPO.to_string()));
        // global: always the home, even in a clone or with -R; the repo is only the context for G
        assert_eq!(run(Global, None, Some("x/y"), Some("x/y")), (global(Some("x/y")), 0, 1), "no API lookup");
        assert_eq!(run(Global, Some("o/r"), Some("x/y"), Some("x/y")), (global(Some("o/r")), 0, 0));
        assert_eq!(run(Global, None, None, None), (global(None), 0, 1));
    }

    #[test]
    fn mode_names_parse() {
        assert_eq!(StartMode::parse("auto"), Some(StartMode::Auto));
        assert_eq!(StartMode::parse("repo"), Some(StartMode::Repo));
        assert_eq!(StartMode::parse("global"), Some(StartMode::Global));
        assert_eq!(StartMode::parse("Global"), None);
        assert_eq!(StartMode::parse(""), None);
    }
}
