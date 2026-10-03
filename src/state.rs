//! Local, per-user state that isn't configuration: which files of which PR head you marked viewed.
//! `$XDG_STATE_HOME/gh-pulse/viewed.json` (default `~/.local/state/...`), keyed by repo + PR number and
//! tied to the head sha, so new commits reset the marks. No GitHub write happens.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Oldest entries are dropped past this many PRs.
const MAX_PRS: usize = 500;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Stored {
    pub sha: String,
    pub files: Vec<String>,
    /// Unix seconds of the last change (for pruning).
    pub t: u64,
}

#[derive(Default)]
pub struct Viewed {
    map: BTreeMap<String, Stored>,
    path: Option<PathBuf>,
}

pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local").join("state")))?;
    Some(base.join("gh-pulse").join("viewed.json"))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Viewed {
    pub fn key(repo: &str, number: u64) -> String {
        format!("{}#{number}", repo.to_lowercase())
    }

    /// A missing file is empty; a corrupt one is ignored (returned warning) and replaced on the next save.
    pub fn load(path: Option<PathBuf>) -> (Viewed, Option<String>) {
        let Some(p) = path.clone() else {
            return (Viewed::default(), None);
        };
        match std::fs::read_to_string(&p) {
            Ok(s) => match serde_json::from_str(&s) {
                Ok(map) => (Viewed { map, path }, None),
                Err(e) => (
                    Viewed {
                        map: BTreeMap::new(),
                        path,
                    },
                    Some(format!("ignoring corrupt {}: {e}", p.display())),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
                Viewed {
                    map: BTreeMap::new(),
                    path,
                },
                None,
            ),
            Err(e) => (
                Viewed {
                    map: BTreeMap::new(),
                    path,
                },
                Some(format!("cannot read {}: {e}", p.display())),
            ),
        }
    }

    pub fn is_viewed(&self, key: &str, sha: &str, file: &str) -> bool {
        self.map
            .get(key)
            .is_some_and(|s| s.sha == sha && s.files.iter().any(|f| f == file))
    }

    /// Add GitHub's viewed files to the local marks (never removes any). True when something changed.
    pub fn merge(&mut self, key: &str, sha: &str, files: &[String]) -> Result<bool, String> {
        let before = self.map.clone();
        let e = self.map.entry(key.to_string()).or_default();
        if e.sha != sha {
            *e = Stored {
                sha: sha.to_string(),
                ..Default::default()
            };
        }
        let missing: Vec<&String> = files.iter().filter(|f| !e.files.contains(f)).collect();
        if missing.is_empty() {
            if e.files.is_empty() {
                self.map.remove(key);
            }
            return Ok(false);
        }
        e.files.extend(missing.into_iter().cloned());
        e.t = now();
        match self.save() {
            Ok(()) => Ok(true),
            Err(err) => {
                self.map = before;
                Err(err)
            }
        }
    }

    /// Flip the mark and save atomically. On a failed save the change is undone and the error returned.
    pub fn toggle(&mut self, key: &str, sha: &str, file: &str) -> Result<bool, String> {
        let before = self.map.clone();
        let e = self.map.entry(key.to_string()).or_default();
        if e.sha != sha {
            *e = Stored {
                sha: sha.to_string(),
                ..Default::default()
            }; // new head: marks start over
        }
        let now_on = match e.files.iter().position(|f| f == file) {
            Some(i) => {
                e.files.remove(i);
                false
            }
            None => {
                e.files.push(file.to_string());
                true
            }
        };
        e.t = now();
        if e.files.is_empty() {
            self.map.remove(key);
        }
        while self.map.len() > MAX_PRS {
            let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, v)| v.t)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.map.remove(&oldest);
        }
        match self.save() {
            Ok(()) => Ok(now_on),
            Err(e) => {
                self.map = before;
                Err(e)
            }
        }
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let dir = path.parent().ok_or("bad state path")?;
        std::fs::create_dir_all(dir).map_err(|e| format!("viewed marks not saved: {e}"))?;
        let body = serde_json::to_string_pretty(&self.map).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, body).map_err(|e| format!("viewed marks not saved: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("viewed marks not saved: {e}")
        })
    }
}

#[cfg(test)]
static DIR_OVERRIDE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Tests point the state directory at a temp directory instead of the user's.
#[cfg(test)]
pub fn set_dir(p: Option<PathBuf>) {
    *DIR_OVERRIDE.lock().unwrap() = p;
}

/// The state directory (`$XDG_STATE_HOME/gh-pulse`, default `~/.local/state/gh-pulse`); only absolute
/// paths are honored.
pub fn dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(d) = DIR_OVERRIDE.lock().unwrap().clone() {
        return Some(d);
    }
    let abs = |k: &str| {
        std::env::var_os(k)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let base = abs("XDG_STATE_HOME").or_else(|| abs("HOME").map(|h| h.join(".local/state")))?;
    Some(base.join("gh-pulse"))
}

/// Writes `body` to `path` through a unique, exclusively created `0600` temp file and a rename.
fn write_private_atomic(path: &Path, body: &str) -> Result<(), String> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = path.parent().ok_or("bad state path")?;
    crate::cache::ensure_dir(dir)?;
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}.{n}.tmp", std::process::id()));
    crate::cache::write_private(&tmp, body.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Reads a regular file of ours (never through a symlink); anything else is "no file".
fn read_regular(path: &Path) -> Option<String> {
    crate::cache::read_nofollow(path)
}

/// The last global-home scope (`all`, `favorites`, `org:x`, `repo:a/b`), kept out of config.toml and
/// tied to the host it was chosen on (another host gets the default).
pub fn load_scope(path: &Path, host: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct S {
        host: String,
        scope: String,
    }
    let s: S = serde_json::from_str(&read_regular(path)?).ok()?;
    (s.host == host).then_some(s.scope)
}

pub fn save_scope(path: &Path, host: &str, scope: &str) -> Result<(), String> {
    write_private_atomic(
        path,
        &serde_json::json!({ "host": host, "scope": scope }).to_string(),
    )
}

/// The repos you entered last, newest first (at most [`RECENT_MAX`]), per host so hosts never mix.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    pub host: String,
    pub repos: Vec<String>,
}

pub const RECENT_MAX: usize = 20;

impl Recent {
    pub fn new(host: &str) -> Recent {
        Recent {
            host: host.into(),
            repos: vec![],
        }
    }

    /// A missing, corrupt or other-host file is simply empty (and replaced on the next save).
    pub fn load(path: &Path, host: &str) -> Recent {
        let mut r: Recent = read_regular(path)
            .and_then(|t| serde_json::from_str(&t).ok())
            .filter(|r: &Recent| r.host == host)
            .unwrap_or_else(|| Recent::new(host));
        r.repos.retain(|x| valid_repo(x));
        r.repos.truncate(RECENT_MAX);
        r
    }

    /// Moves `repo` to the front.
    pub fn touch(&mut self, repo: &str) {
        self.repos.retain(|r| !r.eq_ignore_ascii_case(repo));
        self.repos.insert(0, repo.to_string());
        self.repos.truncate(RECENT_MAX);
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        write_private_atomic(
            path,
            &serde_json::to_string(self).map_err(|e| e.to_string())?,
        )
    }
}

/// `owner/name` with only the characters GitHub allows: the one check every name that reaches a
/// search query or a path goes through.
pub fn valid_repo(s: &str) -> bool {
    let ok = |p: &str| {
        !p.is_empty()
            && p.len() <= 100
            && p != "."
            && p != ".."
            && !p.starts_with('-')
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    s.split_once('/')
        .is_some_and(|(o, n)| ok(o) && ok(n) && !n.contains('/'))
}

#[cfg(test)]
mod tests_recent {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gh-pulse-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn recent_keeps_twenty_newest_first_without_duplicates() {
        let mut r = Recent::new("github.com");
        for i in 0..25 {
            r.touch(&format!("o/r{i}"));
        }
        r.touch("O/R20");
        assert_eq!(r.repos.len(), RECENT_MAX);
        assert_eq!(r.repos[0], "O/R20");
        assert_eq!(
            r.repos
                .iter()
                .filter(|x| x.eq_ignore_ascii_case("o/r20"))
                .count(),
            1
        );
        assert_eq!(r.repos[1], "o/r24");
    }

    #[cfg(unix)]
    #[test]
    fn recent_and_scope_files_are_private_atomic_and_forgiving() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("files");
        let p = d.join("recent.json");
        let mut r = Recent::new("github.com");
        r.touch("o/a");
        r.save(&p).unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(Recent::load(&p, "github.com"), r);
        assert!(
            Recent::load(&p, "ghe.example.com").repos.is_empty(),
            "another host's list is not mixed in"
        );
        // no temp files left behind; a second save replaces the first
        r.touch("o/b");
        r.save(&p).unwrap();
        let names: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        // corrupt or hostile content is ignored
        std::fs::write(&p, "{ not json").unwrap();
        assert!(Recent::load(&p, "github.com").repos.is_empty());
        std::fs::write(
            &p,
            r#"{"host":"github.com","repos":["o/ok","a b/c","../x/y","o/r --repo=z"]}"#,
        )
        .unwrap();
        assert_eq!(Recent::load(&p, "github.com").repos, ["o/ok"]);
        // a symlink where the file should be is not read
        let target = d.join("elsewhere.json");
        std::fs::write(&target, r#"{"host":"github.com","repos":["o/planted"]}"#).unwrap();
        let link = d.join("link.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(Recent::load(&link, "github.com").repos.is_empty());
        // scope round trip
        let sp = d.join("scope.json");
        assert_eq!(load_scope(&sp, "github.com"), None);
        save_scope(&sp, "github.com", "org:cli").unwrap();
        assert_eq!(load_scope(&sp, "github.com").as_deref(), Some("org:cli"));
        assert_eq!(
            load_scope(&sp, "ghe.example.com"),
            None,
            "another host never inherits a scope"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn the_state_directory_is_private_ours_and_never_a_symlink() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let root = tmp("private");
        let d = root.join("nested/gh-pulse");
        save_scope(&d.join("scope.json"), "github.com", "all").unwrap();
        assert_eq!(mode(&d), 0o700, "created private");
        assert_eq!(mode(&d.join("scope.json")), 0o600);
        // ours but loose: tightened by the next save
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        save_scope(&d.join("scope.json"), "github.com", "favorites").unwrap();
        assert_eq!(mode(&d), 0o700);
        // a symlink where the directory should be: nothing is written through it
        let target = root.join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        let link = root.join("link");
        symlink(&target, &link).unwrap();
        let e = save_scope(&link.join("scope.json"), "github.com", "all").unwrap_err();
        assert!(e.contains("not a plain directory"), "{e}");
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
        // somebody else's directory is refused (the file system root is not ours)
        let e = Recent::new("github.com").save(Path::new("/gh-pulse-recent.json"));
        assert!(e.is_err());
        // reads go through the same checks: a file, ours, not a link
        assert!(crate::cache::read_nofollow(&d.join("scope.json")).is_some());
        assert!(crate::cache::read_nofollow(&link).is_none(), "a symlink");
        assert!(crate::cache::read_nofollow(&d).is_none(), "a directory");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn repo_names_are_checked_before_they_reach_a_query() {
        for ok in ["cli/cli", "a-b/c_d.e", "o/r"] {
            assert!(valid_repo(ok), "{ok}");
        }
        for bad in [
            "",
            "cli",
            "a/b/c",
            "a b/c",
            "o/r label:bug",
            "o/",
            "/r",
            "o/r\n",
            "o/r;x",
        ] {
            assert!(!valid_repo(bad), "{bad:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gh-pulse-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("viewed.json")
    }

    #[test]
    fn marks_round_trip_and_reset_on_a_new_head_sha() {
        let p = tmp("rt");
        let (mut v, warn) = Viewed::load(Some(p.clone()));
        assert!(warn.is_none());
        let k = Viewed::key("Octo/Repo", 7);
        assert_eq!(k, "octo/repo#7", "repo names are case-insensitive");
        assert!(v.toggle(&k, "sha1", "a.rs").unwrap());
        assert!(v.toggle(&k, "sha1", "b.rs").unwrap());
        assert!(
            !p.with_extension("json.tmp").exists(),
            "temp file renamed away"
        );
        let (v2, _) = Viewed::load(Some(p.clone()));
        assert!(v2.is_viewed(&k, "sha1", "a.rs") && v2.is_viewed(&k, "sha1", "b.rs"));
        assert!(
            !v2.is_viewed(&k, "sha2", "a.rs"),
            "a new head sha shows nothing viewed"
        );
        assert!(
            !v2.is_viewed(&Viewed::key("octo/repo", 8), "sha1", "a.rs"),
            "keyed by PR number"
        );
        let mut v3 = v2;
        assert!(
            v3.toggle(&k, "sha2", "a.rs").unwrap(),
            "marking on the new head starts a fresh set"
        );
        assert!(!v3.is_viewed(&k, "sha1", "b.rs"));
        assert!(!v3.toggle(&k, "sha2", "a.rs").unwrap(), "toggle off");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn corrupt_or_unwritable_state_never_crashes() {
        let p = tmp("bad");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{ not json").unwrap();
        let (mut v, warn) = Viewed::load(Some(p.clone()));
        assert!(warn.unwrap().contains("ignoring corrupt"));
        assert!(!v.is_viewed("o/r#1", "s", "f"));
        assert!(
            v.toggle("o/r#1", "s", "f").unwrap(),
            "a corrupt file is replaced by a good one"
        );
        assert!(Viewed::load(Some(p.clone())).1.is_none());
        // unwritable location: the toggle is reported and undone, not a panic
        let blocked = p.parent().unwrap().join("file");
        std::fs::write(&blocked, "x").unwrap();
        let (mut v, _) = Viewed::load(Some(blocked.join("sub").join("viewed.json")));
        assert!(v.toggle("o/r#1", "s", "f").is_err());
        assert!(
            !v.is_viewed("o/r#1", "s", "f"),
            "failed save leaves the mark off"
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn merging_github_marks_adds_without_removing() {
        let p = tmp("merge");
        let (mut v, _) = Viewed::load(Some(p.clone()));
        let k = Viewed::key("o/r", 3);
        v.toggle(&k, "s1", "local.rs").unwrap();
        assert!(
            v.merge(&k, "s1", &["gh.rs".into(), "local.rs".into()])
                .unwrap()
        );
        assert!(v.is_viewed(&k, "s1", "gh.rs") && v.is_viewed(&k, "s1", "local.rs"));
        assert!(
            !v.merge(&k, "s1", &["gh.rs".into()]).unwrap(),
            "nothing new, nothing written"
        );
        assert!(
            v.merge(&k, "s2", &["only-new.rs".into()]).unwrap(),
            "a new head starts over with GitHub's marks"
        );
        assert!(!v.is_viewed(&k, "s2", "local.rs") && v.is_viewed(&k, "s2", "only-new.rs"));
        let (v2, _) = Viewed::load(Some(p.clone()));
        assert!(v2.is_viewed(&k, "s2", "only-new.rs"), "persisted");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
