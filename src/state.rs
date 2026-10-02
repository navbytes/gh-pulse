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
}
