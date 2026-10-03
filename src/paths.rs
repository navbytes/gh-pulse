use std::path::{Path, PathBuf};

/// Keep using existing data until a new directory is created explicitly.
pub(crate) fn data_dir(base: &Path) -> PathBuf {
    let current = base.join("gh-tui");
    if std::fs::symlink_metadata(&current).is_ok() {
        return current;
    }
    let legacy = base.join("gh-pulse");
    if std::fs::symlink_metadata(&legacy).is_ok() {
        legacy
    } else {
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_legacy_and_precedence() {
        let root = std::env::temp_dir().join(format!("gh-tui-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(data_dir(&root), root.join("gh-tui"));
        std::fs::create_dir(root.join("gh-pulse")).unwrap();
        assert_eq!(data_dir(&root), root.join("gh-pulse"));
        std::fs::create_dir(root.join("gh-tui")).unwrap();
        assert_eq!(data_dir(&root), root.join("gh-tui"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn existing_legacy_symlink_is_selected_but_cache_rejects_it() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("gh-tui-legacy-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let legacy = root.join("gh-pulse");
        symlink(&root, &legacy).unwrap();
        assert_eq!(data_dir(&root), legacy);
        assert!(crate::cache::validate(&legacy).is_err());
        std::fs::remove_file(legacy).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
