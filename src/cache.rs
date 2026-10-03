//! A small on-disk cache for slow-changing facts (the repo list, a repo's header facts) plus the
//! directory `gh` keeps its own `--cache` entries in. Files are 0600 in a 0700 directory under
//! `$XDG_CACHE_HOME/gh-pulse` (default `~/.cache/gh-pulse`). They hold repo names and public-ish
//! metadata only: no tokens, nothing from write actions, notifications, comments or PR details.
use serde::{Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Relaxed)
}

#[cfg(test)]
static DIR_OVERRIDE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Tests point the cache at a temp directory instead of the user's.
#[cfg(test)]
pub fn set_dir(p: Option<PathBuf>) {
    *DIR_OVERRIDE.lock().unwrap() = p;
}

/// `$XDG_CACHE_HOME/gh-pulse`, or `~/.cache/gh-pulse`. Relative paths in either variable are ignored.
pub fn dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(d) = DIR_OVERRIDE.lock().unwrap().clone() {
        return Some(d);
    }
    dir_from(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))
}

fn dir_from(xdg: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let abs = |v: Option<std::ffi::OsString>| v.map(PathBuf::from).filter(|p| p.is_absolute());
    let base = abs(xdg).or_else(|| abs(home).map(|h| h.join(".cache")))?;
    Some(base.join("gh-pulse"))
}

static WARNING: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Why the cache switched itself off, once (for the status line).
pub fn take_warning() -> Option<String> {
    WARNING.lock().ok().and_then(|mut w| w.take())
}

/// Turn the cache off for this run and remember why.
fn refuse(why: String) {
    set_enabled(false);
    if let Ok(mut w) = WARNING.lock() {
        w.get_or_insert(format!("cache disabled: {why}"));
    }
}

/// The user id we run as (`id -u`; unknown means the cache is not trusted).
#[cfg(unix)]
fn my_uid() -> Option<u32> {
    static UID: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *UID.get_or_init(|| {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
    })
}

/// A real directory (not a symlink) that we own; permissions are tightened to 0700 when they aren't.
/// Ok(false): it does not exist.
fn validate(dir: &Path) -> Result<bool, String> {
    let m = match std::fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    if m.file_type().is_symlink() || !m.is_dir() {
        return Err(format!("{} is not a plain directory", dir.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if my_uid() != Some(m.uid()) {
            return Err(format!("{} belongs to another user", dir.display()));
        }
        if m.mode() & 0o777 != 0o700 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("{}: {e}", dir.display()))?;
        }
    }
    Ok(true)
}

/// What `gh` is told is `XDG_CACHE_HOME` for a cached call (it appends `/gh`); created private.
pub fn gh_home() -> Option<PathBuf> {
    let d = dir()?;
    match Store::new(d.clone()).ensure() {
        Ok(()) => Some(d),
        Err(e) => {
            refuse(e);
            None
        }
    }
}

/// Deletes everything gh-pulse cached (`gh-pulse --clear-cache`); returns what was removed. Only a
/// directory that passes the ownership checks is ever touched.
pub fn clear() -> Result<Option<PathBuf>, String> {
    let Some(d) = dir() else { return Ok(None) };
    if !validate(&d)? {
        return Ok(None);
    }
    std::fs::remove_dir_all(&d)
        .map(|()| Some(d.clone()))
        .map_err(|e| format!("{}: {e}", d.display()))
}

pub struct Store {
    dir: PathBuf,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Entry<T> {
    /// Epoch seconds when it was stored.
    at: u64,
    data: T,
}

/// Entries older than this are never shown, whatever the caller's TTL.
const HARD_MAX_AGE: u64 = 7 * 24 * 3600;

impl Store {
    pub fn new(dir: PathBuf) -> Store {
        Store { dir }
    }

    /// The default store, `None` when caching is off or there is no home directory.
    pub fn default_if_enabled() -> Option<Store> {
        enabled().then(dir).flatten().map(Store::new)
    }

    fn ensure(&self) -> Result<(), String> {
        if validate(&self.dir)? {
            return Ok(());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.dir)
                .map_err(|e| format!("{}: {e}", self.dir.display()))?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        validate(&self.dir).map(|_| ())
    }

    fn file(&self, key: &str) -> PathBuf {
        let safe: String = key
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(format!("{safe}.json"))
    }

    /// (value, age in seconds) as of `now`. A missing, unreadable or corrupt file is simply no entry.
    pub fn read<T: DeserializeOwned>(&self, key: &str, now: u64) -> Option<(T, u64)> {
        let path = self.file(key);
        // a planted symlink or someone else's file is not ours to read
        let m = std::fs::symlink_metadata(&path).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if my_uid() != Some(m.uid()) {
                return None;
            }
        }
        if !m.is_file() {
            return None;
        }
        let text = std::fs::read_to_string(path).ok()?;
        let e: Entry<T> = serde_json::from_str(&text).ok()?;
        let age = now.saturating_sub(e.at);
        (age <= HARD_MAX_AGE).then_some((e.data, age))
    }

    /// Only when younger than `ttl` seconds.
    pub fn fresh<T: DeserializeOwned>(&self, key: &str, ttl: u64, now: u64) -> Option<T> {
        self.read(key, now)
            .filter(|(_, age)| *age <= ttl)
            .map(|(d, _)| d)
    }

    pub fn write<T: Serialize>(&self, key: &str, data: &T, now: u64) -> Result<(), String> {
        if let Err(e) = self.ensure() {
            refuse(e.clone());
            return Err(e);
        }
        let body = serde_json::to_string(&Entry { at: now, data }).map_err(|e| e.to_string())?;
        let path = self.file(key);
        // unique per process and call, and created exclusively: it can't be a pre-planted link
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let tmp = path.with_extension(format!(
            "{}.{}.tmp",
            std::process::id(),
            N.fetch_add(1, Relaxed)
        ));
        write_private(&tmp, body.as_bytes()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    // create_new = O_CREAT|O_EXCL: it fails on anything already there, a symlink included
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)?.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> Store {
        let d = std::env::temp_dir().join(format!("gh-pulse-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        Store::new(d)
    }

    #[test]
    fn entries_expire_by_ttl_but_stay_readable_as_stale() {
        let s = store("ttl");
        s.write("repos", &vec!["a/b".to_string()], 1000).unwrap();
        assert_eq!(
            s.fresh::<Vec<String>>("repos", 600, 1500),
            Some(vec!["a/b".into()])
        );
        assert_eq!(
            s.fresh::<Vec<String>>("repos", 600, 1601),
            None,
            "past its TTL"
        );
        let (v, age) = s.read::<Vec<String>>("repos", 1601).unwrap();
        assert_eq!(
            (v, age),
            (vec!["a/b".to_string()], 601),
            "stale-while-revalidate can still show it"
        );
        assert!(
            s.read::<Vec<String>>("repos", 1000 + HARD_MAX_AGE + 1)
                .is_none(),
            "never shown past a week"
        );
        assert!(s.read::<Vec<String>>("other", 1000).is_none());
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn a_corrupt_or_foreign_file_is_ignored_and_replaced() {
        let s = store("corrupt");
        s.ensure().unwrap();
        std::fs::write(s.file("meta"), "{ not json").unwrap();
        assert!(s.read::<Vec<String>>("meta", 5).is_none());
        std::fs::write(s.file("meta"), r#"{"at":"soon","data":1}"#).unwrap();
        assert!(s.read::<Vec<String>>("meta", 5).is_none(), "wrong shape");
        s.write("meta", &vec![1u8], 5).unwrap();
        assert_eq!(s.read::<Vec<u8>>("meta", 6).map(|x| x.0), Some(vec![1]));
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let s = store("mode");
        s.write("repos/we ird", &1u8, 1).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&s.dir), 0o700);
        assert_eq!(mode(&s.file("repos/we ird")), 0o600);
        assert!(
            s.file("repos/we ird").ends_with("repos_we_ird.json"),
            "keys can't escape the directory"
        );
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[cfg(unix)]
    #[test]
    fn clear_cache_removes_everything_gh_pulse_cached() {
        let shim = crate::testshim::Shim::new();
        let d = dir().unwrap();
        assert!(shim.dir.join("cache") == d);
        assert_eq!(clear().unwrap(), None, "nothing cached yet");
        Store::new(d.clone()).write("repos", &1u8, 1).unwrap();
        std::fs::create_dir_all(d.join("gh/ab")).unwrap();
        std::fs::write(d.join("gh/ab/entry"), "gh's own cached response").unwrap();
        assert_eq!(clear().unwrap(), Some(d.clone()));
        assert!(!d.exists());
        assert_eq!(clear().unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn disabled_cache_reads_and_writes_nothing() {
        let _shim = crate::testshim::Shim::new();
        set_enabled(false);
        assert!(Store::default_if_enabled().is_none(), "[api] cache = false");
        set_enabled(true);
        assert!(Store::default_if_enabled().is_some());
    }

    #[test]
    fn relative_cache_homes_are_ignored() {
        let os = |s: &str| Some(std::ffi::OsString::from(s));
        assert_eq!(
            dir_from(os("/x/cache"), os("/h")),
            Some("/x/cache/gh-pulse".into())
        );
        assert_eq!(
            dir_from(os("rel/cache"), os("/h")),
            Some("/h/.cache/gh-pulse".into())
        );
        assert_eq!(
            dir_from(os(""), os("/h")),
            Some("/h/.cache/gh-pulse".into())
        );
        assert_eq!(
            dir_from(None, os("relative-home")),
            None,
            "no usable home: no cache"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_directory_must_be_ours_and_private() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let s = store("owner");
        // exists with loose permissions and ours: tightened, not refused
        std::fs::create_dir_all(&s.dir).unwrap();
        std::fs::set_permissions(&s.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        s.ensure().unwrap();
        assert_eq!(
            std::fs::metadata(&s.dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // somebody else's directory (the root of the file system is not ours): refused
        if my_uid() != Some(0) {
            assert!(
                validate(Path::new("/"))
                    .unwrap_err()
                    .contains("another user")
            );
        }
        // a symlink where the directory should be: refused, and --clear-cache won't follow it
        let target = store("target");
        std::fs::create_dir_all(&target.dir).unwrap();
        std::fs::write(target.dir.join("precious"), "x").unwrap();
        let link = std::env::temp_dir().join(format!("gh-pulse-cache-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        symlink(&target.dir, &link).unwrap();
        assert!(
            Store::new(link.clone())
                .ensure()
                .unwrap_err()
                .contains("not a plain directory")
        );
        let _shim = crate::testshim::Shim::new();
        set_dir(Some(link.clone()));
        assert!(clear().is_err());
        assert!(
            target.dir.join("precious").exists(),
            "nothing behind the link was touched"
        );
        // a failed write switches the cache off with a reason for the status line
        assert!(Store::new(link.clone()).write("k", &1u8, 1).is_err());
        assert!(!enabled());
        assert!(take_warning().unwrap().starts_with("cache disabled: "));
        assert!(take_warning().is_none());
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&target.dir);
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[cfg(unix)]
    #[test]
    fn planted_files_are_not_read_and_temp_files_are_exclusive() {
        use std::os::unix::fs::symlink;
        let s = store("plant");
        s.write("meta", &7u8, 10).unwrap();
        // a symlink where an entry should be, pointing at something that parses
        let elsewhere = s.dir.join("elsewhere.json");
        std::fs::write(&elsewhere, r#"{"at":10,"data":9}"#).unwrap();
        symlink(&elsewhere, s.file("planted")).unwrap();
        assert!(
            s.read::<u8>("planted", 11).is_none(),
            "symlinks are not followed"
        );
        assert_eq!(s.read::<u8>("meta", 11).map(|x| x.0), Some(7));
        // a pre-planted temp file is never written through
        for i in 0..5 {
            s.write("k", &i, 1).unwrap();
        }
        let leftovers: Vec<_> = std::fs::read_dir(&s.dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files are renamed away");
        let t = s.dir.join("t.tmp");
        std::fs::write(&t, "mine").unwrap();
        assert!(
            write_private(&t, b"x").is_err(),
            "create_new refuses an existing file"
        );
        let _ = std::fs::remove_dir_all(&s.dir);
    }
}
