//! Opening the config file in the user's editor: finding the editor, creating the file from the
//! commented template on first use, and running the editor with the terminal handed over to it.
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// `$VISUAL`, else `$EDITOR`, else `vi` (the order git uses). Empty values do not count.
pub fn command_from(visual: Option<String>, editor: Option<String>) -> String {
    [visual, editor]
        .into_iter()
        .flatten()
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| "vi".to_string())
}

pub fn command() -> String {
    command_from(std::env::var("VISUAL").ok(), std::env::var("EDITOR").ok())
}

/// Create the config from the commented template when it does not exist yet (never overwriting
/// anything, a file made in the meantime included). Ok(true) when it was created.
pub fn ensure_file(path: &Path) -> Result<bool, String> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut o = std::fs::OpenOptions::new();
    // create_new = O_CREAT|O_EXCL: it fails on anything already there, a symlink included
    o.write(true).create_new(true);
    match o.open(path) {
        Ok(mut f) => f
            .write_all(crate::config::template().as_bytes())
            .map(|()| true)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Run `editor` on `path` and wait for it. The editor setting may carry arguments (`code --wait`), so
/// a shell reads it, like git does; the path is passed as an argument and is never part of the script.
/// Only the user's own environment reaches that shell: nothing from GitHub does.
pub fn edit_with(editor: &str, path: &Path) -> Result<(), String> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(path)
        .status()
        .map_err(|e| format!("could not start {editor}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{editor} exited with {status}"))
    }
}

pub fn edit(path: &Path) -> Result<(), String> {
    edit_with(&command(), path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("gh-tui-editor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_editor_is_visual_then_editor_then_vi() {
        let s = |x: &str| Some(x.to_string());
        assert_eq!(command_from(s("code --wait"), s("nano")), "code --wait");
        assert_eq!(command_from(None, s("nano")), "nano");
        assert_eq!(command_from(s("  "), s("nano")), "nano", "blank is unset");
        assert_eq!(command_from(s(""), None), "vi");
        assert_eq!(command_from(None, None), "vi");
    }

    #[test]
    fn a_first_edit_creates_the_commented_template_and_never_overwrites() {
        let d = dir("ensure");
        let path = d.join("nested").join("config.toml");
        assert_eq!(ensure_file(&path), Ok(true));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            crate::config::template()
        );
        // the user's edits are never replaced by the template
        std::fs::write(&path, "ascii = true\n").unwrap();
        assert_eq!(ensure_file(&path), Ok(false));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ascii = true\n");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn the_editor_runs_with_the_path_as_an_argument_and_failures_are_reported() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("run");
        // an "editor" with arguments: appends its first argument's setting to the file it is given
        let script = d.join("ed.sh");
        std::fs::write(&script, "#!/bin/sh\necho \"$1\" >> \"$2\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // the path has a space and a quote in it: it must stay one argument and never become script
        let path = d.join("my 'config'.toml");
        std::fs::write(&path, "").unwrap();
        edit_with(&format!("{} 'ascii = true'", script.display()), &path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ascii = true\n");
        // a failing editor and a missing one are errors, not silent
        assert!(edit_with("false", &path).unwrap_err().contains("exited"));
        assert!(edit_with("no-such-editor-xyz", &path).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
