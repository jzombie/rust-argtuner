use std::path::{Path, PathBuf};

use crate::constants::CONFIG_FILENAME;
use walkdir::WalkDir;

const SKIPPED_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "artifacts",
    "venv",
    ".venv",
    "__pycache__",
    "dist",
    "build",
];

/// Recursively locate argtuner projects (directories containing
/// `argtuner.toml`) under `root`.
///
/// Hidden directories (dot-prefixed) and common build/cache dirs are skipped.
/// Symlinks are not followed (walkdir's default), so cyclic symlink loops
/// cannot cause infinite recursion. Found projects are not descended into.
/// Results are sorted for deterministic output; unreadable directories are
/// skipped best-effort.
pub fn find_projects(root: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let mut walker = WalkDir::new(root).into_iter();
    loop {
        let entry = match walker.next() {
            Some(Ok(entry)) => entry,
            Some(Err(_)) => continue,
            None => break,
        };
        if !entry.file_type().is_dir() {
            continue;
        }
        // Only prune *child* dirs: the root entry (depth 0, name "." or an
        // absolute path) must never be filtered, else `find .` returns nothing.
        let name = entry.file_name().to_string_lossy();
        if entry.depth() > 0 && (name.starts_with('.') || SKIPPED_DIRS.contains(&name.as_ref())) {
            walker.skip_current_dir();
            continue;
        }
        if entry.path().join(CONFIG_FILENAME).is_file() {
            results.push(entry.path().to_path_buf());
            walker.skip_current_dir();
        }
    }
    results.sort();
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn write_config(dir: &Path) {
        fs::write(dir.join(CONFIG_FILENAME), "").unwrap();
    }

    #[test]
    fn finds_nested_projects() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let a = root.join("a");
        let b = root.join("nested").join("b");
        let non_project = root.join("plain");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::create_dir_all(&non_project).unwrap();
        write_config(&a);
        write_config(&b);
        fs::write(non_project.join("readme.md"), "hi").unwrap();

        let found = find_projects(root);
        assert_eq!(found, vec![a, b]);
    }

    #[test]
    fn skips_noise_and_hidden_dirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let project = root.join("proj");
        let in_target = root.join("target").join("proj");
        let hidden = root.join(".hidden").join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&in_target).unwrap();
        fs::create_dir_all(&hidden).unwrap();
        write_config(&project);
        write_config(&in_target);
        write_config(&hidden);

        assert_eq!(find_projects(root), vec![project]);
    }

    #[test]
    fn empty_when_no_projects() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("x/y")).unwrap();
        assert!(find_projects(tmp.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loop_terminates() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let project = root.join("proj");
        fs::create_dir_all(&project).unwrap();
        write_config(&project);
        symlink(root, root.join("loop")).unwrap();

        let found = find_projects(root);
        assert_eq!(found, vec![project]);
    }
}
