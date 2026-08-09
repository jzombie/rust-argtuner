use std::path::{Path, PathBuf};

use crate::constants::CONFIG_FILENAME;
use ignore::WalkBuilder;

/// Recursively locate argtuner projects (directories containing
/// `argtuner.toml`) under `root`.
///
/// The `ignore` crate (ripgrep's engine) does the pruning: it natively
/// respects `.gitignore`, `.ignore`, git exclude, and global git config, skips
/// hidden files/dirs by default, and does not follow symlinks (so cyclic
/// symlink loops cannot cause infinite recursion). No directory names are
/// hardcoded. Unreadable entries are skipped best-effort. Results are sorted
/// and nested projects (a project inside another project's tree) are collapsed
/// to the outermost one.
pub fn find_projects(root: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    // Defaults already give hidden(true), parents(true), git_ignore(true),
    // git_global(true), git_exclude(true), ignore(true) — i.e. .gitignore/.ignore
    // rules apply. require_git is forced off so git-ignore rules are honored
    // even outside a git repository (discovery should not depend on `git init`).
    let walker = WalkBuilder::new(root).require_git(false).build();
    for result in walker {
        let Ok(entry) = result else {
            continue;
        };
        let is_config = entry.file_type().is_some_and(|ft| ft.is_file())
            && entry.file_name().to_str() == Some(CONFIG_FILENAME);
        if is_config && let Some(parent) = entry.path().parent() {
            results.push(parent.to_path_buf());
        }
    }
    results.sort();
    // Nested projects: keep a path only if it is not under an already-found
    // one. Sorting guarantees parents precede children; starts_with is
    // component-aware (no "/a" vs "/ab" false positives).
    let mut dedup = Vec::new();
    for path in results {
        if !dedup
            .iter()
            .any(|parent: &PathBuf| path.starts_with(parent))
        {
            dedup.push(path);
        }
    }
    dedup
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
    fn respects_gitignore_and_hidden() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let project = root.join("proj");
        let in_target = root.join("target").join("proj");
        let in_custom = root.join("out").join("proj");
        let hidden = root.join(".hidden").join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&in_target).unwrap();
        fs::create_dir_all(&in_custom).unwrap();
        fs::create_dir_all(&hidden).unwrap();
        write_config(&project);
        write_config(&in_target);
        write_config(&in_custom);
        write_config(&hidden);
        fs::write(root.join(".gitignore"), "target/\nout/\n").unwrap();

        // target/ and out/ are git-ignored (out/ is not hardcoded anywhere);
        // .hidden/ is skipped by default.
        assert_eq!(find_projects(root), vec![project]);
    }

    #[test]
    fn respects_dot_ignore() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let project = root.join("proj");
        let in_custom = root.join("custom").join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&in_custom).unwrap();
        write_config(&project);
        write_config(&in_custom);
        fs::write(root.join(".ignore"), "custom/\n").unwrap();

        assert_eq!(find_projects(root), vec![project]);
    }

    #[test]
    fn empty_when_no_projects() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("x/y")).unwrap();
        assert!(find_projects(tmp.path()).is_empty());
    }

    #[test]
    fn nested_project_collapses_to_outermost() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let outer = root.join("outer");
        let inner = outer.join("inner");
        fs::create_dir_all(&inner).unwrap();
        write_config(&outer);
        write_config(&inner);

        assert_eq!(find_projects(root), vec![outer]);
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
