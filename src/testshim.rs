//! A fake `gh` for tests: a shell script that records its calls and answers from files in its
//! directory (`<name>.out` for success, `<name>.err` for a failure). One test at a time owns it,
//! because the program name and the shared quota state are process-wide.
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

static LOCK: Mutex<()> = Mutex::new(());

pub struct Shim {
    pub dir: PathBuf,
    _guard: MutexGuard<'static, ()>,
}

const SCRIPT: &str = r#"#!/bin/sh
d="${0%/*}"
echo "$*" >> "$d/calls.log"
echo "${XDG_CACHE_HOME:-}" >> "$d/env.log"
if [ -f "$d/sleep" ]; then read -r s < "$d/sleep"; exec sleep "$s"; fi
pick() {
  if [ -f "$d/$1.err" ]; then cat "$d/$1.err" >&2; exit 1; fi
  if [ -f "$d/$1.out" ]; then cat "$d/$1.out"; exit 0; fi
  echo "[]"; exit 0
}
case "$*" in
  *"api rate_limit"*) pick rate;;
  *"api graphql"*)
    n=$(grep -c "api graphql" "$d/calls.log")
    if [ -f "$d/graphql.$n.out" ]; then cat "$d/graphql.$n.out"; exit 0; fi
    pick graphql;;
  *"api notifications"*) pick notifications;;
  *"api user"*) echo octocat; exit 0;;
  *"search prs"*) pick searchprs;;
  *"search issues"*) pick searchissues;;
  *"pr list"*) pick prlist;;
  *"issue list"*) pick issuelist;;
  *"run list"*) pick runlist;;
  *"repo view"*) pick repoview;;
  *) pick rest;;
esac
"#;

impl Shim {
    pub fn new() -> Shim {
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // jobs left in the shared pool by an earlier test must not run against this test's shim:
        // let them drain first, against a `gh` that fails at once
        crate::gh::set_program(None);
        for _ in 0..1000 {
            if crate::pool::global().idle() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let dir = std::env::temp_dir().join(format!(
            "gh-tui-shim-{}-{}",
            std::process::id(),
            crate::rate::now()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("gh");
        std::fs::write(&script, SCRIPT).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        crate::gh::set_program(Some(script.into_os_string()));
        crate::rate::reset_for_test();
        crate::cache::set_dir(Some(dir.join("cache")));
        crate::cache::set_enabled(true);
        crate::gh::set_identity(Some(("github.com".into(), "octocat".into())));
        crate::state::set_dir(Some(dir.join("state")));
        Shim { dir, _guard: guard }
    }

    pub fn set(&self, name: &str, body: &str) {
        std::fs::write(self.dir.join(name), body).unwrap();
    }

    pub fn clear(&self, name: &str) {
        let _ = std::fs::remove_file(self.dir.join(name));
    }

    /// Each `gh` invocation's arguments, in order.
    pub fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    /// `XDG_CACHE_HOME` as each invocation saw it ("" when unset).
    pub fn envs(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("env.log"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }
}

impl Drop for Shim {
    fn drop(&mut self) {
        crate::gh::set_program(None);
        crate::rate::reset_for_test();
        crate::cache::set_dir(None);
        crate::cache::set_slow(3600);
        crate::rate::set_limits(20, 10);
        crate::gh::set_timeout(60);
        crate::gh::clear_identity();
        crate::state::set_dir(None);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A `gh api rate_limit` document with these graphql numbers (core is full).
pub fn rate_doc(graphql_remaining: u32, reset: u64) -> String {
    format!(
        r#"{{"resources":{{"core":{{"limit":5000,"remaining":5000,"reset":{reset}}},"graphql":{{"limit":5000,"remaining":{graphql_remaining},"reset":{reset}}},"search":{{"limit":30,"remaining":30,"reset":{reset}}}}}}}"#
    )
}
